// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// The main window: the 设备 page (sharing switch, arrangement, the user's
// devices), the 设置 page (MainWindow.Settings.cpp) and, in development
// builds, the edge animation page (MainWindow.Effect.cpp). Everything shown
// comes from the agent's state snapshots; edits go back as requests.

#include "pch.h"
#include "MainWindow.xaml.h"
#if __has_include("MainWindow.g.cpp")
#include "MainWindow.g.cpp"
#endif
#include "PeerItem.h"
#include "AppShell.h"
#include "AppIcon.h"
#include "Strings.h"

// Fully qualified: WIL declares a global ::Microsoft namespace as well.
using namespace winrt;
using namespace winrt::Microsoft::UI::Xaml;
using namespace winrt::Microsoft::UI::Xaml::Controls;
using namespace winrt::Windows::Data::Json;

namespace
{
#if defined(_DEBUG)
    // The edge animation is a development aid; release builds leave it out.
    constexpr bool DevelopmentBuild = true;
#else
    constexpr bool DevelopmentBuild = false;
#endif

    // What a row shows, to skip rebuilding rows that did not change. The
    // latency changes all the time and is updated in place instead.
    std::wstring RowSignature(Rinflow::PeerItem const& row)
    {
        std::wstring signature;
        for (auto const& part : { row.Id(), row.Name(), row.Detail(), row.Glyph(), row.StatusText() })
        {
            signature += part;
            signature += L'\x1f';
        }
        signature += row.IsConnected() ? L'c' : L'-';
        signature += row.IsOffline() ? L'o' : L'-';
        signature += row.CanConnect() ? L'k' : L'-';
        return signature;
    }

    // Updates `target` to `rows`, replacing only the rows that changed when
    // the devices are the same, so the list does not flicker every second.
    void UpdateRows(Windows::Foundation::Collections::IObservableVector<Windows::Foundation::IInspectable> const& target,
        std::vector<Rinflow::PeerItem> const& rows)
    {
        bool sameIds = target.Size() == rows.size();
        for (uint32_t i = 0; sameIds && i < rows.size(); ++i)
        {
            sameIds = target.GetAt(i).as<Rinflow::PeerItem>().Id() == rows[i].Id();
        }
        if (sameIds)
        {
            for (uint32_t i = 0; i < rows.size(); ++i)
            {
                auto current = target.GetAt(i).as<Rinflow::PeerItem>();
                if (RowSignature(current) != RowSignature(rows[i]))
                {
                    target.SetAt(i, rows[i]);
                }
                else
                {
                    // An open menu or the pointer over the row stays put.
                    current.Latency(rows[i].Latency());
                }
            }
            return;
        }
        std::vector<Windows::Foundation::IInspectable> items(rows.begin(), rows.end());
        target.ReplaceAll(items);
    }
}

namespace winrt::Rinflow::implementation
{
    void MainWindow::InitializeComponent()
    {
        MainWindowT::InitializeComponent();
        Root().Language(RinflowApp::LanguageTag());

        ExtendsContentIntoTitleBar(true);
        SetTitleBar(AppTitleBar());

        // Opens centred on the work area of the display Windows picked for it
        // (the one it was launched from), shrunk to fit a small screen.
        HWND hwnd{};
        check_hresult(m_inner.as<::IWindowNative>()->get_WindowHandle(&hwnd));
        ::RinflowApp::KeepWindowIconThemed(hwnd, AppWindow());
        auto monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        MONITORINFO info{ sizeof(info) };
        GetMonitorInfoW(monitor, &info);
        UINT dpiX = 96, dpiY = 96;
        if (FAILED(GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &dpiX, &dpiY))) dpiX = GetDpiForWindow(hwnd);
        double scale = dpiX / 96.0;
        auto const& work = info.rcWork;
        int32_t workW = work.right - work.left;
        int32_t workH = work.bottom - work.top;
        int32_t w = (std::min)(static_cast<int32_t>(std::lround(720 * scale)), workW);
        int32_t h = (std::min)(static_cast<int32_t>(std::lround(680 * scale)), workH);
        AppWindow().MoveAndResize({ work.left + (workW - w) / 2, work.top + (workH - h) / 2, w, h });

        auto dispatcher = DispatcherQueue();
        auto weak = get_weak();
        m_agent = std::make_unique<::RinflowApp::AgentClient>();
        m_agent->Start(
            [dispatcher, weak](JsonObject const& message)
            {
                dispatcher.TryEnqueue([weak, message]
                {
                    if (auto self = weak.get(); self && !self->m_closed) self->ApplyMessage(message);
                });
            },
            [dispatcher, weak](bool linked)
            {
                dispatcher.TryEnqueue([weak, linked]
                {
                    if (auto self = weak.get(); self && !self->m_closed) self->SetAgentLinked(linked);
                });
            },
            [dispatcher, weak](::RinflowApp::AgentProblem problem, uint32_t code)
            {
                dispatcher.TryEnqueue([weak, problem, code]
                {
                    if (auto self = weak.get(); self && !self->m_closed) self->ShowAgentProblem(problem, code);
                });
            });
        AgentRetryButton().Click([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (!self) return;
            self->AgentBar().IsOpen(false);
            self->m_agent->Retry();
        });

        SetupDevicesPage();
        SetupArrangement();
        SetupSettingsPage();
        SetupEffectPage();
        SetupPairing();
        SetupTransfers();

        BackButton().Click([weak](auto&&, auto&&)
        {
            if (auto self = weak.get()) self->ShowPage(Page::Devices);
        });
        SettingsButton().Click([weak](auto&&, auto&&)
        {
            if (auto self = weak.get()) self->ShowPage(Page::Settings);
        });
        EffectButton().Visibility(DevelopmentBuild ? Visibility::Visible : Visibility::Collapsed);
        EffectButton().Click([weak](auto&&, auto&&)
        {
            if (auto self = weak.get()) self->ShowPage(Page::Effect);
        });

        ErrorBar().CloseButtonClick([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (!self) return;
            if (self->m_errorFromAgent) self->m_agent->ClearError();
            self->m_errorFromAgent = false;
        });

        Closed([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (!self) return;
            self->m_closed = true;
            // 关闭窗口时保留在托盘 off: closing the window quits Rinflow.
            if (!self->m_keepInTray && !RinflowApp::IsRelaunching())
            {
                self->m_agent->Quit();
            }
            self->m_agent->Stop();
        });
    }

    void MainWindow::ShowPage(Page page)
    {
        m_page = page;
        auto show = [](FrameworkElement const& element, bool visible)
        {
            element.Visibility(visible ? Visibility::Visible : Visibility::Collapsed);
        };
        show(DevicesPage(), page == Page::Devices);
        show(SettingsPage(), page == Page::Settings);
        show(EffectPage(), page == Page::Effect && DevelopmentBuild);
        show(BackButton(), page != Page::Devices);
        if (page == Page::Devices && m_layoutDirty && !m_drag && !m_settling)
        {
            RenderArrangement();
        }
    }

    void MainWindow::SetupDevicesPage()
    {
        auto weak = get_weak();
        SharingToggle().Toggled([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (!self) return;
            bool on = self->SharingToggle().IsOn();
            self->SharingState().Text(RinflowApp::Text(on ? L"On" : L"Off"));
            if (!self->m_applying)
            {
                self->m_agent->UpdateSetting(L"sharing", JsonValue::CreateBooleanValue(on));
            }
        });
        AddDeviceButton().Click([weak](auto&&, auto&&)
        {
            if (auto self = weak.get()) self->ShowAddDevice();
        });
        AddDeviceDialog().Closed([weak](auto&&, auto&&)
        {
            if (auto self = weak.get()) self->m_addDeviceOpen = false;
        });
    }

    void MainWindow::ShowAddDevice()
    {
        if (m_addDeviceOpen)
        {
            return;
        }
        m_addDeviceOpen = true;
        // Another dialog (pairing, the shortcut) may still be closing: then
        // it does not open, and the button works again.
        auto failed = [weak = get_weak()]
        {
            if (auto self = weak.get()) self->m_addDeviceOpen = false;
        };
        try
        {
            AddDeviceDialog().XamlRoot(Root().XamlRoot());
            AddDeviceDialog().ShowAsync().Completed([failed](auto const&, Windows::Foundation::AsyncStatus status)
            {
                if (status == Windows::Foundation::AsyncStatus::Error) failed();
            });
        }
        catch (hresult_error const&)
        {
            failed();
        }
    }

    void MainWindow::OnConnectDevice(Windows::Foundation::IInspectable const& sender, RoutedEventArgs const&)
    {
        auto peerId = unbox_value_or<hstring>(sender.as<FrameworkElement>().Tag(), L"");
        if (!peerId.empty())
        {
            m_agent->Connect(peerId);
        }
    }

    void MainWindow::OnDeviceMore(Windows::Foundation::IInspectable const& sender, RoutedEventArgs const&)
    {
        auto button = sender.as<FrameworkElement>();
        auto peerId = unbox_value_or<hstring>(button.Tag(), L"");
        if (auto device = m_model.Find(peerId))
        {
            DeviceMenu(*device).ShowAt(button);
        }
    }

    MenuFlyout MainWindow::DeviceMenu(RinflowApp::DeviceInfo const& device)
    {
        MenuFlyout menu;
        auto weak = get_weak();
        auto peerId = device.id;
        auto add = [&](hstring const& text, wchar_t const* glyph, std::function<void(MainWindow&)> action)
        {
            MenuFlyoutItem item;
            item.Text(text);
            if (glyph)
            {
                FontIcon icon;
                icon.Glyph(glyph);
                item.Icon(icon);
            }
            item.Click([weak, action](auto&&, auto&&)
            {
                if (auto self = weak.get()) action(*self);
            });
            menu.Items().Append(item);
        };

        if (device.connected)
        {
            add(RinflowApp::Text(L"MenuDisconnect"), L"\xE8CD", [peerId](MainWindow& self) { self.m_agent->Disconnect(peerId); });
        }
        else if (device.online && !device.connecting)
        {
            add(RinflowApp::Text(L"MenuConnect"), L"\xE8CE", [peerId](MainWindow& self) { self.m_agent->Connect(peerId); });
        }
        if (menu.Items().Size() > 0)
        {
            menu.Items().Append(MenuFlyoutSeparator());
        }
        add(RinflowApp::Text(L"MenuPlaceLeft"), nullptr, [peerId](MainWindow& self) { self.m_agent->PlaceBeside(peerId, L"left"); });
        add(RinflowApp::Text(L"MenuPlaceRight"), nullptr, [peerId](MainWindow& self) { self.m_agent->PlaceBeside(peerId, L"right"); });
        add(RinflowApp::Text(L"MenuPlaceTop"), nullptr, [peerId](MainWindow& self) { self.m_agent->PlaceBeside(peerId, L"top"); });
        add(RinflowApp::Text(L"MenuPlaceBottom"), nullptr, [peerId](MainWindow& self) { self.m_agent->PlaceBeside(peerId, L"bottom"); });
        if (device.placed)
        {
            add(RinflowApp::Text(L"MenuUnplace"), nullptr, [peerId](MainWindow& self) { self.m_agent->RemovePlacement(peerId); });
        }
        menu.Items().Append(MenuFlyoutSeparator());
        auto deviceName = device.name;
        add(RinflowApp::Text(L"MenuPeerInput"), L"\xE962", [peerId, deviceName](MainWindow& self) { self.EditPeerInput(peerId, deviceName); });
        auto name = device.name;
        add(RinflowApp::Text(L"MenuForget"), L"\xE74D", [peerId, name](MainWindow& self)
        {
            ContentDialog confirm;
            confirm.XamlRoot(self.Root().XamlRoot());
            confirm.Title(box_value(RinflowApp::Format(L"ForgetTitle", std::wstring(name))));
            confirm.Content(box_value(RinflowApp::Text(L"ForgetContent")));
            confirm.PrimaryButtonText(RinflowApp::Text(L"ForgetConfirm"));
            confirm.CloseButtonText(RinflowApp::Text(L"Cancel"));
            confirm.DefaultButton(ContentDialogButton::Close);
            auto weak = self.get_weak();
            try
            {
                // Not shown while another dialog is open (or closing).
                confirm.ShowAsync().Completed([weak, peerId](auto const& result, Windows::Foundation::AsyncStatus status)
                {
                    if (status != Windows::Foundation::AsyncStatus::Completed
                        || result.GetResults() != ContentDialogResult::Primary) return;
                    if (auto self = weak.get(); self && !self->m_closed) self->m_agent->ForgetDevice(peerId);
                });
            }
            catch (hresult_error const&)
            {
            }
        });
        return menu;
    }

    void MainWindow::ApplyMessage(JsonObject const& message)
    {
        auto type = message.GetNamedString(L"type", L"");
        if (type == L"state")
        {
            ApplyState(message);
        }
        else if (type == L"placement_preview")
        {
            ApplyPlacementPreview(message);
        }
        else if (type == L"error")
        {
            // A request failed: shown until closed or replaced.
            ShowError(message.GetNamedString(L"message", L""), false);
        }
    }

    void MainWindow::ApplyState(JsonObject const& state)
    {
        // The user quit Rinflow (the agent's tray): every window goes too.
        if (state.GetNamedBoolean(L"quitting", false))
        {
            RinflowApp::QuitApp();
            return;
        }
        m_localName = state.GetNamedString(L"device_name", L"Rinflow");
        m_deviceId = state.GetNamedString(L"device_id", L"");
        auto error = state.GetNamedString(L"last_error", L"");
        if (error != m_agentError)
        {
            m_agentError = error;
            if (!error.empty()) ShowError(error, true);
            else if (m_errorFromAgent) ShowError(L"", true);
        }
        ApplySettings(state);
        ApplyDevices(state);
        ApplyTransfers(state);
        ApplyAddresses(state);
        ApplyLayout(state);
        ApplyEffect(state);
        m_lastState = state;
        ApplyPairing(state);
    }

    void MainWindow::ApplyDevices(JsonObject const& state)
    {
        m_model = RinflowApp::DeviceModel::FromState(state);
        m_connectedPeerIds.clear();
        hstring controller;
        size_t connected = 0;
        bool connecting = false;
        for (auto const& device : m_model.mine)
        {
            if (device.connected)
            {
                m_connectedPeerIds.insert(std::wstring(device.id));
                ++connected;
                if (device.controlling) controller = device.name;
            }
            connecting = connecting || device.connecting;
        }

        std::vector<Rinflow::PeerItem> rows;
        for (auto const& device : m_model.mine) rows.push_back(RinflowApp::DeviceRow(device));
        UpdateRows(m_devices, rows);
        DevicesEmptyText().Visibility(rows.empty() ? Visibility::Visible : Visibility::Collapsed);

        std::vector<Rinflow::PeerItem> nearby;
        for (auto const& device : m_model.nearby) nearby.push_back(RinflowApp::NearbyRow(device));
        UpdateRows(m_nearby, nearby);
        NearbyEmptyPanel().Visibility(nearby.empty() ? Visibility::Visible : Visibility::Collapsed);

        // The dot by 添加设备: green while connected; the tooltip says more.
        StatusDotConnected().Visibility(connected > 0 ? Visibility::Visible : Visibility::Collapsed);
        hstring status;
        if (!controller.empty()) status = RinflowApp::Format(L"StatusControlledBy", std::wstring(controller));
        else if (connected == 1) status = RinflowApp::Text(L"StatusOneConnected");
        else if (connected > 1) status = RinflowApp::Format(L"StatusManyConnected", connected);
        else if (connecting) status = RinflowApp::Text(L"StatusConnecting");
        else
        {
            auto core = state.GetNamedString(L"core", L"starting");
            status = core == L"running" ? RinflowApp::Text(L"StatusSearching")
                : core == L"stopped" ? RinflowApp::Text(L"StatusCoreStopped")
                : RinflowApp::Text(L"StatusStarting");
        }
        ToolTipService::SetToolTip(StatusIndicator(), box_value(status));
    }

    void MainWindow::ApplyTheme(hstring const& theme)
    {
        if (theme == m_theme)
        {
            return;
        }
        m_theme = theme;
        auto requested = theme == L"light" ? ElementTheme::Light
            : theme == L"dark" ? ElementTheme::Dark
            : ElementTheme::Default;
        Root().RequestedTheme(requested);
        AppWindow().TitleBar().PreferredTheme(theme == L"light" ? Microsoft::UI::Windowing::TitleBarTheme::Light
            : theme == L"dark" ? Microsoft::UI::Windowing::TitleBarTheme::Dark
            : Microsoft::UI::Windowing::TitleBarTheme::UseDefaultAppMode);
        // Code-drawn parts pick their colours up again.
        m_layoutSignature.clear();
        RenderDots();
    }

    void MainWindow::SetAgentLinked(bool linked)
    {
        if (!linked)
        {
            ToolTipService::SetToolTip(StatusIndicator(), box_value(RinflowApp::Text(L"StatusAgentConnecting")));
            StatusDotConnected().Visibility(Visibility::Collapsed);
            m_devices.Clear();
            m_nearby.Clear();
            m_connectedPeerIds.clear();
            DevicesEmptyText().Visibility(Visibility::Visible);
            // A restarted agent may have different values; reload them.
            m_effectRevision = -1;
            m_layoutSignature.clear();
            m_agentError = L"";
            m_transferSignature.clear();
            m_transferRows.clear();
            TransfersPanel().Children().Clear();
            TransfersPanel().Visibility(Visibility::Collapsed);
            // A preview asked of the agent that went away is never answered.
            m_previewPending = false;
            m_previewQueued = false;
        }
    }

    void MainWindow::ShowAgentProblem(::RinflowApp::AgentProblem problem, uint32_t code)
    {
        using ::RinflowApp::AgentProblem;
        if (problem == AgentProblem::None)
        {
            AgentBar().IsOpen(false);
            return;
        }
        // Exit codes and NTSTATUS values read best in hex (0xC0000135 is a
        // missing DLL), Win32 errors in decimal (2: file not found).
        auto hex = std::format(L"0x{:X}", code);
        hstring message = problem == AgentProblem::CannotStart
            ? RinflowApp::Format(L"AgentCannotStart", std::to_wstring(code))
            : problem == AgentProblem::Exited
            ? RinflowApp::Format(L"AgentExited", hex)
            : RinflowApp::Text(L"AgentNoAnswer");
        AgentBar().Title(RinflowApp::Text(L"AgentProblemTitle"));
        AgentBar().Message(message);
        AgentBar().IsOpen(true);
        // Not "no devices yet": the devices are unknown while it is down.
        DevicesEmptyText().Visibility(Visibility::Collapsed);
    }

    void MainWindow::ShowError(hstring const& message, bool fromAgent)
    {
        m_errorFromAgent = fromAgent && !message.empty();
        ErrorBar().Message(message);
        ErrorBar().IsOpen(!message.empty());
    }
}
