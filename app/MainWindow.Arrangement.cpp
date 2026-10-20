// The 排列 page: this PC's displays and the peers around them, drawn to scale
// in logical points. Peers are placed by dragging; the agent's core snaps
// every drop to a valid spot (touching an edge, enough overlap) and syncs it
// to the peer, so this page only proposes positions and draws what comes back.

#include "pch.h"
#include "MainWindow.xaml.h"

// Fully qualified: WIL declares a global ::Microsoft namespace as well.
using namespace winrt;
using namespace winrt::Microsoft::UI::Xaml;
using namespace winrt::Microsoft::UI::Xaml::Controls;
using namespace winrt::Microsoft::UI::Xaml::Input;
using namespace winrt::Microsoft::UI::Xaml::Media;
using namespace winrt::Windows::Data::Json;
using winrt::Windows::Foundation::Point;

namespace
{
    using LayoutRect = winrt::LinkCursor::implementation::MainWindow::LayoutRect;

    Brush ThemeBrush(wchar_t const* key)
    {
        return Application::Current().Resources().Lookup(box_value(key)).as<Brush>();
    }

    LayoutRect ParseRect(JsonObject const& value)
    {
        return { value.GetNamedNumber(L"x", 0), value.GetNamedNumber(L"y", 0),
            value.GetNamedNumber(L"width", 0), value.GetNamedNumber(L"height", 0) };
    }

    LayoutRect BoundsOf(std::vector<LayoutRect> const& rects)
    {
        if (rects.empty())
        {
            return { 0, 0, 1440, 900 };
        }
        double left = rects[0].x, top = rects[0].y, right = rects[0].Right(), bottom = rects[0].Bottom();
        for (auto const& rect : rects)
        {
            left = (std::min)(left, rect.x);
            top = (std::min)(top, rect.y);
            right = (std::max)(right, rect.Right());
            bottom = (std::max)(bottom, rect.Bottom());
        }
        return { left, top, right - left, bottom - top };
    }

    LayoutRect Union(LayoutRect const& a, LayoutRect const& b)
    {
        return BoundsOf({ a, b });
    }

    hstring SideLabel(hstring const& side)
    {
        if (side == L"left") return L"在左边";
        if (side == L"right") return L"在右边";
        if (side == L"top") return L"在上边";
        if (side == L"bottom") return L"在下边";
        return L"";
    }

    Shapes::Rectangle Screen(double width, double height, Brush const& fill, Brush const& stroke,
        double thickness, bool dashed)
    {
        Shapes::Rectangle rectangle;
        rectangle.Width((std::max)(width, 2.0));
        rectangle.Height((std::max)(height, 2.0));
        rectangle.RadiusX(6);
        rectangle.RadiusY(6);
        rectangle.Fill(fill);
        rectangle.Stroke(stroke);
        rectangle.StrokeThickness(thickness);
        if (dashed)
        {
            DoubleCollection dashes;
            dashes.Append(4);
            dashes.Append(3);
            rectangle.StrokeDashArray(dashes);
        }
        return rectangle;
    }

    TextBlock Label(hstring const& text, Brush const& foreground, bool strong)
    {
        TextBlock label;
        label.Text(text);
        label.Foreground(foreground);
        label.FontSize(strong ? 13 : 11);
        if (strong)
        {
            label.FontWeight(Windows::UI::Text::FontWeights::SemiBold());
        }
        label.TextTrimming(TextTrimming::CharacterEllipsis);
        label.HorizontalAlignment(HorizontalAlignment::Center);
        label.TextAlignment(TextAlignment::Center);
        return label;
    }
}

namespace winrt::LinkCursor::implementation
{
    void MainWindow::SetupArrangement()
    {
        auto weak = get_weak();
        ArrangementHost().SizeChanged([weak](auto&&, auto&&)
        {
            if (auto self = weak.get(); self && !self->m_drag) self->RenderArrangement();
        });
    }

    MainWindow::LayoutDevice const* MainWindow::FindDevice(hstring const& peerId) const
    {
        for (auto const& device : m_layoutDevices)
        {
            if (device.peerId == peerId) return &device;
        }
        return nullptr;
    }

    void MainWindow::ApplyLayout(JsonObject const& state)
    {
        auto layout = state.GetNamedObject(L"layout", JsonObject{});
        std::set<std::wstring> online;
        for (auto const& value : state.GetNamedArray(L"peers", JsonArray{}))
        {
            online.insert(std::wstring(value.GetObject().GetNamedString(L"id", L"")));
        }

        // Redraw only when something drawn changed, not on every snapshot.
        std::wstring signature{ layout.Stringify() };
        for (auto const& id : online) signature += L"|o:" + id;
        for (auto const& id : m_connectedPeerIds) signature += L"|c:" + id;
        signature += L"|n:" + std::wstring(DeviceNameText().Text());
        if (signature == m_layoutSignature)
        {
            return;
        }
        m_layoutSignature = signature;
        m_onlinePeerIds = std::move(online);

        m_localDisplays.clear();
        for (auto const& value : layout.GetNamedArray(L"displays", JsonArray{}))
        {
            m_localDisplays.push_back(ParseRect(value.GetObject()));
        }
        m_layoutDevices.clear();
        for (auto const& value : layout.GetNamedArray(L"devices", JsonArray{}))
        {
            auto entry = value.GetObject();
            LayoutDevice device;
            device.peerId = entry.GetNamedString(L"peer_id", L"");
            device.name = entry.GetNamedString(L"name", device.peerId);
            device.platform = entry.GetNamedString(L"platform", L"");
            for (auto const& display : entry.GetNamedArray(L"displays", JsonArray{}))
            {
                device.displays.push_back(ParseRect(display.GetObject()));
            }
            device.bounds = BoundsOf(device.displays);
            auto offset = entry.GetNamedValue(L"offset", JsonValue::CreateNullValue());
            if (offset.ValueType() == JsonValueType::Object)
            {
                auto point = offset.GetObject();
                device.offset = std::make_pair(point.GetNamedNumber(L"x", 0), point.GetNamedNumber(L"y", 0));
            }
            auto side = entry.GetNamedValue(L"side", JsonValue::CreateNullValue());
            if (side.ValueType() == JsonValueType::String)
            {
                device.side = side.GetString();
            }
            m_layoutDevices.push_back(std::move(device));
        }

        if (m_drag)
        {
            m_layoutDirty = true;
            return;
        }
        RenderArrangement();
    }

    void MainWindow::RenderArrangement()
    {
        auto canvas = ArrangementCanvas();
        double width = ArrangementHost().ActualWidth();
        double height = ArrangementHost().ActualHeight();
        if (width <= 1 || height <= 1)
        {
            m_layoutDirty = true;
            return;
        }
        m_layoutDirty = false;
        canvas.Children().Clear();

        // Fit this PC plus generous room around it, and every placed peer.
        auto local = BoundsOf(m_localDisplays);
        double room = (std::max)(local.width, local.height) * 0.75;
        LayoutRect view{ local.x - room, local.y - room, local.width + 2 * room, local.height + 2 * room };
        for (auto const& device : m_layoutDevices)
        {
            if (device.offset)
            {
                LayoutRect placed{ device.bounds.x + device.offset->first, device.bounds.y + device.offset->second,
                    device.bounds.width, device.bounds.height };
                view = Union(view, placed);
            }
        }
        double padding = 12;
        m_layoutScale = (std::min)((width - 2 * padding) / view.width, (height - 2 * padding) / view.height);
        m_layoutOriginX = view.x - (width / m_layoutScale - view.width) / 2;
        m_layoutOriginY = view.y - (height / m_layoutScale - view.height) / 2;
        auto toCanvasX = [this](double x) { return (x - m_layoutOriginX) * m_layoutScale; };
        auto toCanvasY = [this](double y) { return (y - m_layoutOriginY) * m_layoutScale; };

        // This PC: each monitor on its own, the primary one (at the origin)
        // labelled with the PC's name.
        int number = 1;
        for (auto const& display : m_localDisplays)
        {
            Grid screen;
            screen.Width(display.width * m_layoutScale);
            screen.Height(display.height * m_layoutScale);
            screen.Children().Append(Screen(display.width * m_layoutScale, display.height * m_layoutScale,
                ThemeBrush(L"AccentFillColorDefaultBrush"), ThemeBrush(L"AccentFillColorSecondaryBrush"), 1, false));
            bool primary = display.x == 0 && display.y == 0;
            StackPanel labels;
            labels.VerticalAlignment(VerticalAlignment::Center);
            labels.Children().Append(Label(primary ? DeviceNameText().Text() : hstring(std::format(L"显示器 {}", number)),
                ThemeBrush(L"TextOnAccentFillColorPrimaryBrush"), true));
            if (primary)
            {
                labels.Children().Append(Label(L"这台电脑", ThemeBrush(L"TextOnAccentFillColorSecondaryBrush"), false));
            }
            screen.Children().Append(labels);
            Canvas::SetLeft(screen, toCanvasX(display.x));
            Canvas::SetTop(screen, toCanvasY(display.y));
            canvas.Children().Append(screen);
            ++number;
        }

        // Placed peers.
        for (auto const& device : m_layoutDevices)
        {
            if (!device.offset) continue;
            auto element = DeviceElement(device, false);
            Canvas::SetLeft(element, toCanvasX(device.bounds.x + device.offset->first));
            Canvas::SetTop(element, toCanvasY(device.bounds.y + device.offset->second));
            canvas.Children().Append(element);
        }

        // Discovered peers that are not placed yet wait in the tray.
        auto tray = UnplacedPanel();
        tray.Children().Clear();
        auto weak = get_weak();
        for (auto const& device : m_layoutDevices)
        {
            if (device.offset || !m_onlinePeerIds.contains(std::wstring(device.peerId))) continue;
            Border chip;
            chip.Padding(ThicknessHelper::FromLengths(14, 6, 14, 6));
            chip.CornerRadius(CornerRadiusHelper::FromUniformRadius(14));
            chip.Background(ThemeBrush(L"ControlFillColorDefaultBrush"));
            chip.BorderBrush(ThemeBrush(L"ControlStrokeColorDefaultBrush"));
            chip.BorderThickness(ThicknessHelper::FromUniformLength(1));
            TextBlock name;
            name.Text(device.name);
            chip.Child(name);
            // No tooltip: it would pop up over the device being dragged, and
            // the hint under the tray already says what to do.
            auto peerId = device.peerId;
            chip.PointerPressed([weak, peerId](IInspectable const& sender, PointerRoutedEventArgs const& args)
            {
                auto self = weak.get();
                if (!self || self->m_drag) return;
                auto device = self->FindDevice(peerId);
                if (!device) return;
                // The chip turns into a to-scale ghost of the device under
                // the pointer, which the chip keeps steering while captured.
                auto ghost = self->DeviceElement(*device, true);
                auto position = args.GetCurrentPoint(self->ArrangementCanvas()).Position();
                double ghostWidth = device->bounds.width * self->m_layoutScale;
                double ghostHeight = device->bounds.height * self->m_layoutScale;
                Canvas::SetLeft(ghost, position.X - ghostWidth / 2);
                Canvas::SetTop(ghost, position.Y - ghostHeight / 2);
                self->ArrangementCanvas().Children().Append(ghost);
                self->m_drag = LayoutDrag{ peerId, ghost,
                    Point{ static_cast<float>(ghostWidth / 2), static_cast<float>(ghostHeight / 2) }, true };
                sender.as<UIElement>().CapturePointer(args.Pointer());
                args.Handled(true);
            });
            chip.PointerMoved([weak](auto&&, PointerRoutedEventArgs const& args)
            {
                if (auto self = weak.get()) self->MoveDrag(args);
            });
            chip.PointerReleased([weak](auto&&, PointerRoutedEventArgs const& args)
            {
                if (auto self = weak.get()) self->EndDrag(args);
            });
            tray.Children().Append(chip);
        }
        UnplacedHint().Text(tray.Children().Size() == 0
            ? L"没有等待放置的设备。同一网络中发现的设备会出现在这里；把已放置的设备拖到框外可以取消放置。"
            : L"把设备拖到上面屏幕的旁边。把已放置的设备拖到框外可以取消放置。");
    }

    FrameworkElement MainWindow::DeviceElement(LayoutDevice const& device, bool ghost)
    {
        bool connected = m_connectedPeerIds.contains(std::wstring(device.peerId));
        bool online = m_onlinePeerIds.contains(std::wstring(device.peerId));
        double scale = m_layoutScale;

        Grid group;
        group.Width(device.bounds.width * scale);
        group.Height(device.bounds.height * scale);
        group.Background(SolidColorBrush(Windows::UI::Colors::Transparent()));
        if (ghost) group.Opacity(0.85);
        for (auto const& display : device.displays)
        {
            auto screen = Screen(display.width * scale, display.height * scale,
                online ? ThemeBrush(L"ControlFillColorDefaultBrush") : ThemeBrush(L"ControlFillColorTransparentBrush"),
                connected ? ThemeBrush(L"AccentFillColorDefaultBrush") : ThemeBrush(L"ControlStrokeColorSecondaryBrush"),
                connected ? 2 : 1, !online);
            screen.HorizontalAlignment(HorizontalAlignment::Left);
            screen.VerticalAlignment(VerticalAlignment::Top);
            screen.Margin(ThicknessHelper::FromLengths((display.x - device.bounds.x) * scale,
                (display.y - device.bounds.y) * scale, 0, 0));
            group.Children().Append(screen);
        }
        StackPanel labels;
        labels.VerticalAlignment(VerticalAlignment::Center);
        labels.Padding(ThicknessHelper::FromLengths(4, 0, 4, 0));
        labels.Children().Append(Label(device.name, ThemeBrush(L"TextFillColorPrimaryBrush"), true));
        labels.Children().Append(Label(connected ? L"已连接" : online ? L"未连接" : L"离线",
            ThemeBrush(L"TextFillColorSecondaryBrush"), false));
        group.Children().Append(labels);

        if (!ghost)
        {
            auto weak = get_weak();
            auto peerId = device.peerId;
            group.PointerPressed([weak, peerId](IInspectable const& sender, PointerRoutedEventArgs const& args)
            {
                auto self = weak.get();
                if (!self || self->m_drag) return;
                if (auto device = self->FindDevice(peerId))
                {
                    self->BeginDrag(*device, sender.as<FrameworkElement>(), args, false);
                }
            });
            group.PointerMoved([weak](auto&&, PointerRoutedEventArgs const& args)
            {
                if (auto self = weak.get()) self->MoveDrag(args);
            });
            group.PointerReleased([weak](auto&&, PointerRoutedEventArgs const& args)
            {
                if (auto self = weak.get()) self->EndDrag(args);
            });
            group.PointerCaptureLost([weak](auto&&, auto&&)
            {
                // Capture lost without a release (e.g. the window lost
                // focus): drop the drag and show the stored arrangement.
                auto self = weak.get();
                if (self && self->m_drag && !self->m_drag->fromTray)
                {
                    self->m_drag.reset();
                    self->RenderArrangement();
                }
            });
        }
        return group;
    }

    void MainWindow::BeginDrag(LayoutDevice const& device, FrameworkElement const& element,
        PointerRoutedEventArgs const& args, bool fromTray)
    {
        auto position = args.GetCurrentPoint(ArrangementCanvas()).Position();
        auto left = Canvas::GetLeft(element);
        auto top = Canvas::GetTop(element);
        m_drag = LayoutDrag{ device.peerId, element,
            Point{ static_cast<float>(position.X - left), static_cast<float>(position.Y - top) }, fromTray };
        element.CapturePointer(args.Pointer());
        // Drawn above the other devices while it moves.
        Canvas::SetZIndex(element, 10);
        args.Handled(true);
    }

    void MainWindow::MoveDrag(PointerRoutedEventArgs const& args)
    {
        if (!m_drag) return;
        auto position = args.GetCurrentPoint(ArrangementCanvas()).Position();
        Canvas::SetLeft(m_drag->element, position.X - m_drag->grab.X);
        Canvas::SetTop(m_drag->element, position.Y - m_drag->grab.Y);
        args.Handled(true);
    }

    void MainWindow::EndDrag(PointerRoutedEventArgs const& args)
    {
        if (!m_drag) return;
        auto drag = *m_drag;
        m_drag.reset();
        args.Handled(true);

        auto position = args.GetCurrentPoint(ArrangementCanvas()).Position();
        bool inside = position.X >= 0 && position.Y >= 0
            && position.X <= ArrangementHost().ActualWidth() && position.Y <= ArrangementHost().ActualHeight();
        auto device = FindDevice(drag.peerId);
        if (inside && device)
        {
            // Propose the drop position; the core snaps it and the next
            // snapshot draws where it actually landed.
            double worldX = Canvas::GetLeft(drag.element) / m_layoutScale + m_layoutOriginX;
            double worldY = Canvas::GetTop(drag.element) / m_layoutScale + m_layoutOriginY;
            m_agent->SetPlacement(drag.peerId, worldX - device->bounds.x, worldY - device->bounds.y);
        }
        else if (!drag.fromTray)
        {
            m_agent->RemovePlacement(drag.peerId);
        }

        // Redraw from the stored arrangement shortly, in case the drop changed
        // nothing (dropped back on its spot) and no new snapshot arrives.
        m_layoutSignature.clear();
        auto timer = DispatcherQueue().CreateTimer();
        timer.Interval(std::chrono::milliseconds(250));
        timer.IsRepeating(false);
        auto weak = get_weak();
        timer.Tick([weak](auto&& sender, auto&&)
        {
            sender.Stop();
            if (auto self = weak.get(); self && !self->m_drag) self->RenderArrangement();
        });
        timer.Start();
    }

    void MainWindow::PlaceFromMenu(IInspectable const& sender, wchar_t const* side)
    {
        auto peerId = unbox_value_or<hstring>(sender.as<FrameworkElement>().Tag(), L"");
        if (!peerId.empty()) m_agent->PlaceBeside(peerId, side);
    }

    void MainWindow::OnPlaceLeft(IInspectable const& sender, RoutedEventArgs const&) { PlaceFromMenu(sender, L"left"); }
    void MainWindow::OnPlaceRight(IInspectable const& sender, RoutedEventArgs const&) { PlaceFromMenu(sender, L"right"); }
    void MainWindow::OnPlaceTop(IInspectable const& sender, RoutedEventArgs const&) { PlaceFromMenu(sender, L"top"); }
    void MainWindow::OnPlaceBottom(IInspectable const& sender, RoutedEventArgs const&) { PlaceFromMenu(sender, L"bottom"); }

    void MainWindow::OnRemovePlacement(IInspectable const& sender, RoutedEventArgs const&)
    {
        auto peerId = unbox_value_or<hstring>(sender.as<FrameworkElement>().Tag(), L"");
        if (!peerId.empty()) m_agent->RemovePlacement(peerId);
    }
}
