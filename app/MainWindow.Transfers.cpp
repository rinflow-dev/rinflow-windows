// Three smaller parts of the main window, all driven by the snapshot:
//
// - Files on their way (`transfers`): a card per transfer on the 设备 page,
//   with its progress and a button to stop it.
// - Devices by address (the `manual_addresses` setting, and their state in
//   the snapshot's `manual_addresses`): in the 添加设备 dialog, for networks
//   where devices do not find each other by themselves.
// - How this PC's mouse feels on one device (the `peer_input` setting): the
//   device menu's 鼠标和滚动 dialog.

#include "pch.h"
#include "MainWindow.xaml.h"
#include "Strings.h"

// Fully qualified: WIL declares a global ::Microsoft namespace as well.
using namespace winrt;
using namespace winrt::Microsoft::UI::Xaml;
using namespace winrt::Microsoft::UI::Xaml::Controls;
using namespace winrt::Windows::Data::Json;

namespace
{
    // At most this many addresses (the agent's limit too).
    constexpr uint32_t MaxAddresses = 32;

    std::wstring FormatBytes(double bytes)
    {
        constexpr double KiB = 1024.0, MiB = KiB * 1024.0, GiB = MiB * 1024.0;
        if (bytes >= GiB) return std::format(L"{:.2f} GB", bytes / GiB);
        if (bytes >= MiB) return std::format(L"{:.1f} MB", bytes / MiB);
        if (bytes >= KiB) return std::format(L"{:.0f} KB", bytes / KiB);
        return std::format(L"{:.0f} B", bytes);
    }

    // What a user could type: a host name, an IPv4 address, or an IPv6
    // address in brackets, with an optional port. The agent checks again.
    bool LooksLikeAddress(std::wstring_view text)
    {
        if (text.empty() || text.size() > 259) return false;
        return std::all_of(text.begin(), text.end(), [](wchar_t c)
        {
            return (c >= L'a' && c <= L'z') || (c >= L'A' && c <= L'Z') || (c >= L'0' && c <= L'9')
                || c == L'.' || c == L'-' || c == L'_' || c == L':' || c == L'[' || c == L']' || c == L'%';
        });
    }

    std::wstring Trimmed(hstring const& text)
    {
        std::wstring value{ text };
        auto first = value.find_first_not_of(L" \t\r\n");
        if (first == std::wstring::npos) return {};
        auto last = value.find_last_not_of(L" \t\r\n");
        return value.substr(first, last - first + 1);
    }

    bool SameAddress(std::wstring_view a, std::wstring_view b)
    {
        return a.size() == b.size() && std::equal(a.begin(), a.end(), b.begin(), [](wchar_t x, wchar_t y)
        {
            return towlower(x) == towlower(y);
        });
    }
}

namespace winrt::LinkCursor::implementation
{
    void MainWindow::SetupTransfers()
    {
        auto weak = get_weak();
        AddAddressButton().Click([weak](auto&&, auto&&)
        {
            if (auto self = weak.get()) self->AddAddress();
        });
        AddressBox().KeyDown([weak](auto&&, Input::KeyRoutedEventArgs const& args)
        {
            if (args.Key() != Windows::System::VirtualKey::Enter) return;
            args.Handled(true);
            if (auto self = weak.get()) self->AddAddress();
        });
        AddressBox().TextChanged([weak](auto&&, auto&&)
        {
            if (auto self = weak.get()) self->AddressError().Visibility(Visibility::Collapsed);
        });

        PointerSpeedSlider().ValueChanged([weak](auto&&, auto&&)
        {
            if (auto self = weak.get())
            {
                self->PointerSpeedValue().Text(hstring(std::format(L"{:.2f}×", self->PointerSpeedSlider().Value())));
            }
        });
        ScrollSpeedSlider().ValueChanged([weak](auto&&, auto&&)
        {
            if (auto self = weak.get())
            {
                self->ScrollSpeedValue().Text(hstring(std::format(L"{:.2f}×", self->ScrollSpeedSlider().Value())));
            }
        });
    }

    // ---- Files on their way ------------------------------------------------

    void MainWindow::ApplyTransfers(JsonObject const& state)
    {
        auto transfers = state.GetNamedArray(L"transfers", JsonArray{});
        // Cards are rebuilt only when the transfers themselves change;
        // progress updates in place.
        std::wstring signature;
        for (auto const& value : transfers)
        {
            auto transfer = value.GetObject();
            signature += transfer.GetNamedString(L"direction", L"");
            signature += std::to_wstring(static_cast<uint64_t>(transfer.GetNamedNumber(L"transfer", 0)));
            signature += transfer.GetNamedString(L"peer_name", L"");
            signature += transfer.GetNamedString(L"name", L"");
            signature += transfer.GetNamedBoolean(L"awaiting_acceptance", false) ? L"offered" : L"active";
            signature += L'\x1f';
        }
        if (signature != m_transferSignature)
        {
            m_transferSignature = signature;
            m_transferRows.clear();
            TransfersPanel().Children().Clear();
            for (auto const& value : transfers)
            {
                auto transfer = value.GetObject();
                bool receiving = transfer.GetNamedString(L"direction", L"") == L"receiving";
                bool offered = transfer.GetNamedBoolean(L"awaiting_acceptance", false);
                auto number = static_cast<uint32_t>(transfer.GetNamedNumber(L"transfer", 0));
                auto items = static_cast<uint64_t>(transfer.GetNamedNumber(L"items", 0));
                std::wstring name{ transfer.GetNamedString(L"name", L"") };
                std::wstring peer{ transfer.GetNamedString(L"peer_name", L"") };
                std::wstring what{ items > 1
                    ? LinkCursorApp::Format(L"TransferManyItems", name, items)
                    : LinkCursorApp::Format(L"TransferOneItem", name) };
                auto title = offered ? LinkCursorApp::Format(L"TransferOffered", peer, what) : receiving
                    ? LinkCursorApp::Format(L"TransferReceiving", peer, what)
                    : LinkCursorApp::Format(L"TransferSending", peer, what);

                Grid card;
                card.Style(Application::Current().Resources().Lookup(box_value(L"CardStyle")).as<Style>());
                for (auto width : { GridLengthHelper::Auto(), GridLengthHelper::FromValueAndType(1, GridUnitType::Star), GridLengthHelper::Auto() })
                {
                    ColumnDefinition column;
                    column.Width(width);
                    card.ColumnDefinitions().Append(column);
                }
                FontIcon icon;
                icon.FontSize(20);
                icon.VerticalAlignment(VerticalAlignment::Center);
                icon.Glyph(receiving ? L"\xE896" : L"\xE898"); // Download / Upload
                card.Children().Append(icon);

                StackPanel text;
                text.Spacing(4);
                text.VerticalAlignment(VerticalAlignment::Center);
                Grid::SetColumn(text, 1);
                TextBlock heading;
                heading.Text(title);
                heading.TextTrimming(TextTrimming::CharacterEllipsis);
                text.Children().Append(heading);
                ProgressBar bar;
                bar.Maximum(100);
                text.Children().Append(bar);
                TextBlock detail;
                detail.Style(Application::Current().Resources().Lookup(box_value(L"RowDescriptionStyle")).as<Style>());
                text.Children().Append(detail);
                card.Children().Append(text);

                Button cancel;
                cancel.Style(Application::Current().Resources().Lookup(box_value(L"SubtleIconButtonStyle")).as<Style>());
                cancel.VerticalAlignment(VerticalAlignment::Center);
                FontIcon cancelIcon;
                cancelIcon.FontSize(16);
                cancelIcon.Glyph(L"\xE711");
                cancel.Content(cancelIcon);
                auto stop = LinkCursorApp::Text(L"TransferCancel");
                ToolTipService::SetToolTip(cancel, box_value(stop));
                Automation::AutomationProperties::SetName(cancel, stop);
                Grid::SetColumn(cancel, 2);
                auto weak = get_weak();
                cancel.Click([weak, number](auto&&, auto&&)
                {
                    if (auto self = weak.get()) self->m_agent->CancelTransfer(number);
                });
                card.Children().Append(cancel);

                if (offered)
                {
                    StackPanel actions;
                    actions.Orientation(Orientation::Horizontal);
                    actions.Spacing(8);
                    actions.VerticalAlignment(VerticalAlignment::Center);
                    Grid::SetColumn(actions, 2);
                    Button receive;
                    receive.Content(box_value(LinkCursorApp::Text(L"TransferReceive")));
                    receive.Click([weak, number](auto&&, auto&&)
                    {
                        if (auto self = weak.get()) self->m_agent->AcceptTransfer(number);
                    });
                    actions.Children().Append(receive);
                    card.Children().RemoveAtEnd();
                    actions.Children().Append(cancel);
                    card.Children().Append(actions);
                    bar.Visibility(Visibility::Collapsed);
                }

                TransfersPanel().Children().Append(card);
                m_transferRows[std::wstring(transfer.GetNamedString(L"direction", L"")) + std::to_wstring(number)] = { bar, detail };
            }
        }
        for (auto const& value : transfers)
        {
            auto transfer = value.GetObject();
            auto key = std::wstring(transfer.GetNamedString(L"direction", L""))
                + std::to_wstring(static_cast<uint32_t>(transfer.GetNamedNumber(L"transfer", 0)));
            auto row = m_transferRows.find(key);
            if (row == m_transferRows.end()) continue;
            double done = transfer.GetNamedNumber(L"done", 0);
            double total = transfer.GetNamedNumber(L"total", 0);
            row->second.bar.IsIndeterminate(total <= 0);
            row->second.bar.Value(total > 0 ? done * 100.0 / total : 0);
            row->second.detail.Text(transfer.GetNamedBoolean(L"awaiting_acceptance", false)
                ? LinkCursorApp::Text(L"TransferLegacyHint")
                : hstring(FormatBytes(done) + L" / " + FormatBytes(total)));
        }
        TransfersPanel().Visibility(transfers.Size() > 0 ? Visibility::Visible : Visibility::Collapsed);
    }

    // ---- Devices by address --------------------------------------------------

    std::vector<std::wstring> MainWindow::ManualAddresses() const
    {
        std::vector<std::wstring> addresses;
        if (!m_settings) return addresses;
        for (auto const& value : m_settings.GetNamedArray(L"manual_addresses", JsonArray{}))
        {
            if (value.ValueType() == JsonValueType::String)
            {
                addresses.emplace_back(value.GetString());
            }
        }
        return addresses;
    }

    void MainWindow::SaveAddresses(std::vector<std::wstring> const& addresses)
    {
        JsonArray array;
        for (auto const& address : addresses)
        {
            array.Append(JsonValue::CreateStringValue(address));
        }
        m_agent->UpdateSetting(L"manual_addresses", array);
    }

    void MainWindow::AddAddress()
    {
        auto address = Trimmed(AddressBox().Text());
        auto fail = [this](wchar_t const* key)
        {
            AddressError().Text(LinkCursorApp::Text(key));
            AddressError().Visibility(Visibility::Visible);
        };
        if (address.empty()) return;
        if (!LooksLikeAddress(address)) return fail(L"AddressInvalid");
        auto addresses = ManualAddresses();
        if (std::any_of(addresses.begin(), addresses.end(), [&](auto const& known) { return SameAddress(known, address); }))
        {
            return fail(L"AddressDuplicate");
        }
        if (addresses.size() >= MaxAddresses) return fail(L"AddressTooMany");
        addresses.push_back(address);
        SaveAddresses(addresses);
        AddressBox().Text(L"");
    }

    void MainWindow::ApplyAddresses(JsonObject const& state)
    {
        auto addresses = ManualAddresses();
        auto statuses = state.GetNamedArray(L"manual_addresses", JsonArray{});
        auto statusOf = [&](std::wstring const& address) -> JsonObject
        {
            for (auto const& value : statuses)
            {
                auto status = value.GetObject();
                if (status.GetNamedString(L"address", L"") == address) return status;
            }
            return JsonObject{};
        };
        std::wstring signature;
        for (auto const& address : addresses)
        {
            signature += address + L'\x1e' + std::wstring(statusOf(address).Stringify()) + L'\x1f';
        }
        if (signature == m_addressSignature) return;
        m_addressSignature = signature;

        AddressList().Children().Clear();
        auto weak = get_weak();
        for (auto const& address : addresses)
        {
            auto status = statusOf(address);
            auto kind = status.GetNamedString(L"status", L"searching");
            hstring line;
            if (kind == L"found")
            {
                auto name = status.GetNamedString(L"name", status.GetNamedString(L"peer_id", L""));
                line = LinkCursorApp::Format(L"AddressFound", std::wstring(name));
            }
            else if (kind == L"no_response") line = LinkCursorApp::Text(L"AddressNoResponse");
            else if (kind == L"unresolved")
            {
                line = LinkCursorApp::Format(L"AddressUnresolved", std::wstring(status.GetNamedString(L"error", L"")));
            }
            else line = LinkCursorApp::Text(L"AddressSearching");

            Grid row;
            row.ColumnSpacing(8);
            ColumnDefinition grow;
            grow.Width(GridLengthHelper::FromValueAndType(1, GridUnitType::Star));
            row.ColumnDefinitions().Append(grow);
            row.ColumnDefinitions().Append(ColumnDefinition{});
            row.ColumnDefinitions().GetAt(1).Width(GridLengthHelper::Auto());

            StackPanel text;
            text.VerticalAlignment(VerticalAlignment::Center);
            TextBlock name;
            name.Text(address);
            name.TextTrimming(TextTrimming::CharacterEllipsis);
            text.Children().Append(name);
            TextBlock detail;
            detail.Text(line);
            detail.TextWrapping(TextWrapping::Wrap);
            detail.Style(Application::Current().Resources().Lookup(box_value(L"RowDescriptionStyle")).as<Style>());
            if (kind == L"found")
            {
                detail.Foreground(Application::Current().Resources().Lookup(box_value(L"SystemFillColorSuccessBrush")).as<Media::Brush>());
            }
            text.Children().Append(detail);
            row.Children().Append(text);

            Button remove;
            remove.Style(Application::Current().Resources().Lookup(box_value(L"SubtleIconButtonStyle")).as<Style>());
            remove.VerticalAlignment(VerticalAlignment::Center);
            FontIcon icon;
            icon.FontSize(14);
            icon.Glyph(L"\xE711");
            remove.Content(icon);
            auto removeText = LinkCursorApp::Text(L"AddressRemove");
            ToolTipService::SetToolTip(remove, box_value(removeText));
            Automation::AutomationProperties::SetName(remove, removeText);
            Grid::SetColumn(remove, 1);
            remove.Click([weak, address](auto&&, auto&&)
            {
                auto self = weak.get();
                if (!self) return;
                auto addresses = self->ManualAddresses();
                std::erase_if(addresses, [&](auto const& known) { return SameAddress(known, address); });
                self->SaveAddresses(addresses);
            });
            row.Children().Append(remove);
            AddressList().Children().Append(row);
        }
    }

    // ---- How the mouse feels on one device -----------------------------------

    fire_and_forget MainWindow::EditPeerInput(hstring peerId, hstring name)
    {
        if (m_peerInputOpen) co_return;
        auto strong = get_strong();
        auto all = m_settings ? m_settings.GetNamedObject(L"peer_input", JsonObject{}) : JsonObject{};
        auto current = all.GetNamedObject(peerId, JsonObject{});
        PeerInputDialog().Title(box_value(LinkCursorApp::Format(L"PeerInputTitle", std::wstring(name))));
        PointerSpeedSlider().Value(current.GetNamedNumber(L"pointer_speed", 1.0));
        ScrollSpeedSlider().Value(current.GetNamedNumber(L"scroll_speed", 1.0));
        InvertScrollToggle().IsOn(current.GetNamedBoolean(L"invert_scroll", false));
        PointerSpeedValue().Text(hstring(std::format(L"{:.2f}×", PointerSpeedSlider().Value())));
        ScrollSpeedValue().Text(hstring(std::format(L"{:.2f}×", ScrollSpeedSlider().Value())));

        m_peerInputOpen = true;
        auto result = ContentDialogResult::None;
        try
        {
            PeerInputDialog().XamlRoot(Root().XamlRoot());
            result = co_await PeerInputDialog().ShowAsync();
        }
        catch (hresult_error const&)
        {
            // Another dialog is open (or still closing): nothing changes.
        }
        m_peerInputOpen = false;
        if (m_closed || result == ContentDialogResult::None) co_return;

        // The whole map goes back (the setting is one value).
        auto updated = JsonObject::Parse(all.Stringify());
        if (updated.HasKey(peerId)) updated.Remove(peerId);
        if (result == ContentDialogResult::Primary)
        {
            auto round = [](double value) { return std::round(value * 100.0) / 100.0; };
            JsonObject entry;
            entry.Insert(L"pointer_speed", JsonValue::CreateNumberValue(round(PointerSpeedSlider().Value())));
            entry.Insert(L"scroll_speed", JsonValue::CreateNumberValue(round(ScrollSpeedSlider().Value())));
            entry.Insert(L"invert_scroll", JsonValue::CreateBooleanValue(InvertScrollToggle().IsOn()));
            updated.Insert(peerId, entry);
        }
        m_agent->UpdateSetting(L"peer_input", updated);
    }
}
