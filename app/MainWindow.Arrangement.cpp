// The arrangement on the 设备 page: this PC's screens and the placed peers
// around them, drawn to scale in logical points on a dotted canvas. Peers are
// placed by dragging (from the canvas, or from the 未放置 chips); the agent's
// core snaps every drop to a valid spot (touching an edge, enough overlap)
// and syncs it to the peer, so this page only proposes positions and draws
// what comes back. Dragging a placed peer out of the canvas unplaces it.

#include "pch.h"
#include "MainWindow.xaml.h"
#include "DeviceModel.h"

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

    // Room around the drawing for the chips (top) and the hint (bottom).
    constexpr double PadSide = 24;
    constexpr double PadTop = 52;
    constexpr double PadBottom = 48;
    constexpr double DotSpacing = 22;
    // A press that moves less than this is a click, not a drag.
    constexpr double DragThreshold = 6;

    LayoutRect ParseRect(JsonObject const& value)
    {
        return { value.GetNamedNumber(L"x", 0), value.GetNamedNumber(L"y", 0),
            value.GetNamedNumber(L"width", 0), value.GetNamedNumber(L"height", 0),
            value.GetNamedNumber(L"scale", 1) };
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

    // The main display: the one at the origin of its device, else the first.
    size_t PrimaryIndex(std::vector<LayoutRect> const& displays)
    {
        for (size_t i = 0; i < displays.size(); ++i)
        {
            if (displays[i].x == 0 && displays[i].y == 0) return i;
        }
        return 0;
    }

    hstring Pixels(LayoutRect const& display)
    {
        return hstring(std::format(L"{}×{}", std::lround(display.width * display.scale),
            std::lround(display.height * display.scale)));
    }

    double Distance(Point a, Point b)
    {
        return std::hypot(a.X - b.X, a.Y - b.Y);
    }
}

namespace winrt::LinkCursor::implementation
{
    void MainWindow::SetupArrangement()
    {
        auto weak = get_weak();
        ArrangementHost().SizeChanged([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (!self) return;
            self->RenderDots();
            if (!self->m_drag) self->RenderArrangement();
        });
    }

    MainWindow::LayoutDevice const* MainWindow::FindLayoutDevice(hstring const& peerId) const
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
        signature += L"|n:" + std::wstring(m_localName);
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
            device.remembered = entry.GetNamedBoolean(L"remembered", false);
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

        if (m_drag || m_page != Page::Devices)
        {
            m_layoutDirty = true;
            return;
        }
        RenderArrangement();
    }

    void MainWindow::RenderDots()
    {
        auto canvas = DotsCanvas();
        canvas.Children().Clear();
        double width = ArrangementHost().ActualWidth();
        double height = ArrangementHost().ActualHeight();
        if (width <= 1 || height <= 1)
        {
            return;
        }
        GeometryGroup dots;
        for (double y = DotSpacing / 2; y < height; y += DotSpacing)
        {
            for (double x = DotSpacing / 2; x < width; x += DotSpacing)
            {
                EllipseGeometry dot;
                dot.Center({ static_cast<float>(x), static_cast<float>(y) });
                dot.RadiusX(1.1);
                dot.RadiusY(1.1);
                dots.Children().Append(dot);
            }
        }
        auto path = Root().Resources().Lookup(box_value(L"DotsTemplate")).as<DataTemplate>().LoadContent().as<Shapes::Path>();
        path.Data(dots);
        canvas.Children().Append(path);
    }

    FrameworkElement MainWindow::Screen(hstring const& templateKey, double width, double height,
        hstring const& glyph, hstring const& title, hstring const& subtitle, bool dashed, bool)
    {
        auto screen = Root().Resources().Lookup(box_value(templateKey)).as<DataTemplate>().LoadContent().as<Grid>();
        screen.Width((std::max)(width, 4.0));
        screen.Height((std::max)(height, 4.0));
        auto shape = screen.FindName(L"Shape").as<Shapes::Rectangle>();
        if (dashed)
        {
            DoubleCollection dashes;
            dashes.Append(4);
            dashes.Append(3);
            shape.StrokeDashArray(dashes);
        }
        auto icon = screen.FindName(L"Icon").as<FontIcon>();
        auto titleText = screen.FindName(L"Title").as<TextBlock>();
        auto subtitleText = screen.FindName(L"Subtitle").as<TextBlock>();
        auto labels = screen.FindName(L"Labels").as<StackPanel>();
        labels.MaxWidth((std::max)(width - 8, 0.0));
        icon.Glyph(glyph);
        titleText.Text(title);
        subtitleText.Text(subtitle);
        // Small screens show less.
        icon.Visibility(glyph.empty() || height < 84 ? Visibility::Collapsed : Visibility::Visible);
        titleText.Visibility(title.empty() || height < 24 ? Visibility::Collapsed : Visibility::Visible);
        subtitleText.Visibility(subtitle.empty() || height < 52 ? Visibility::Collapsed : Visibility::Visible);
        return screen;
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

        // Fit this PC with room on every side for placing, and every placed peer.
        auto local = BoundsOf(m_localDisplays);
        double roomX = local.width * 0.8;
        double roomY = local.height * 0.45;
        LayoutRect view{ local.x - roomX, local.y - roomY, local.width + 2 * roomX, local.height + 2 * roomY };
        for (auto const& device : m_layoutDevices)
        {
            if (device.offset)
            {
                LayoutRect placed{ device.bounds.x + device.offset->first, device.bounds.y + device.offset->second,
                    device.bounds.width, device.bounds.height };
                view = Union(view, placed);
            }
        }
        double areaHeight = (std::max)(height - PadTop - PadBottom, 40.0);
        m_layoutScale = (std::min)((width - 2 * PadSide) / view.width, areaHeight / view.height);
        m_layoutOriginX = view.x - (width / m_layoutScale - view.width) / 2;
        m_layoutOriginY = view.y - (areaHeight / m_layoutScale - view.height) / 2 - PadTop / m_layoutScale;
        auto toCanvasX = [this](double x) { return (x - m_layoutOriginX) * m_layoutScale; };
        auto toCanvasY = [this](double y) { return (y - m_layoutOriginY) * m_layoutScale; };

        // This PC: each monitor on its own; the main one carries the name.
        int number = 2;
        auto primary = PrimaryIndex(m_localDisplays);
        for (size_t i = 0; i < m_localDisplays.size(); ++i)
        {
            auto const& display = m_localDisplays[i];
            bool main = i == primary;
            auto screen = Screen(L"LocalScreenTemplate", display.width * m_layoutScale, display.height * m_layoutScale,
                main ? L"\xE7F4" : L"",
                main ? m_localName : hstring(std::format(L"显示器 {}", number)),
                main ? L"本机 · " + Pixels(display) : Pixels(display),
                false, true);
            if (!main) ++number;
            Canvas::SetLeft(screen, toCanvasX(display.x));
            Canvas::SetTop(screen, toCanvasY(display.y));
            canvas.Children().Append(screen);
        }

        // Placed peers.
        bool anyPlaced = false;
        for (auto const& device : m_layoutDevices)
        {
            if (!device.offset) continue;
            anyPlaced = true;
            auto element = DeviceElement(device, false);
            Canvas::SetLeft(element, toCanvasX(device.bounds.x + device.offset->first));
            Canvas::SetTop(element, toCanvasY(device.bounds.y + device.offset->second));
            canvas.Children().Append(element);
        }

        // The user's online devices that are not placed yet wait as chips.
        auto chips = UnplacedChips();
        chips.Children().Clear();
        auto weak = get_weak();
        for (auto const& device : m_layoutDevices)
        {
            auto id = std::wstring(device.peerId);
            bool mine = device.remembered || m_connectedPeerIds.contains(id);
            if (device.offset || !mine || !m_onlinePeerIds.contains(id)) continue;
            auto chip = Root().Resources().Lookup(box_value(L"ChipTemplate")).as<DataTemplate>().LoadContent().as<Border>();
            chip.FindName(L"Icon").as<FontIcon>().Glyph(LinkCursorApp::DeviceGlyph(device.platform));
            chip.FindName(L"Title").as<TextBlock>().Text(device.name);
            // No tooltip: it would pop up over the device being dragged.
            auto peerId = device.peerId;
            chip.PointerPressed([weak, peerId](IInspectable const& sender, PointerRoutedEventArgs const& args)
            {
                auto self = weak.get();
                if (!self || self->m_drag) return;
                auto device = self->FindLayoutDevice(peerId);
                if (!device) return;
                // A to-scale ghost of the device follows the pointer, steered
                // by the chip while it holds the capture.
                auto ghost = self->DeviceElement(*device, true);
                auto position = args.GetCurrentPoint(self->ArrangementCanvas()).Position();
                double ghostWidth = device->bounds.width * self->m_layoutScale;
                double ghostHeight = device->bounds.height * self->m_layoutScale;
                Canvas::SetLeft(ghost, position.X - ghostWidth / 2);
                Canvas::SetTop(ghost, position.Y - ghostHeight / 2);
                ghost.Visibility(Visibility::Collapsed); // until it moves
                self->ArrangementCanvas().Children().Append(ghost);
                self->m_drag = LayoutDrag{ peerId, ghost,
                    Point{ static_cast<float>(ghostWidth / 2), static_cast<float>(ghostHeight / 2) }, position, true };
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
            chip.PointerCaptureLost([weak](auto&&, auto&&)
            {
                auto self = weak.get();
                if (self && self->m_drag && self->m_drag->fromChip)
                {
                    self->m_drag.reset();
                    self->RenderArrangement();
                }
            });
            chips.Children().Append(chip);
        }
        UnplacedPanel().Visibility(chips.Children().Size() > 0 ? Visibility::Visible : Visibility::Collapsed);
        ArrangementHint().Text(anyPlaced || chips.Children().Size() > 0
            ? L"拖动以调整屏幕位置"
            : L"连接设备后，可以在这里把它拖到屏幕旁边");
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
        else if (!online) group.Opacity(0.6);

        auto primary = PrimaryIndex(device.displays);
        for (size_t i = 0; i < device.displays.size(); ++i)
        {
            auto const& display = device.displays[i];
            bool main = i == primary;
            hstring subtitle;
            if (main)
            {
                subtitle = LinkCursorApp::PlatformLabel(device.platform) + L" · " + Pixels(display);
                if (!connected) subtitle = subtitle + (online ? L" · 未连接" : L" · 离线");
            }
            auto screen = Screen(L"PeerScreenTemplate", display.width * scale, display.height * scale,
                main ? LinkCursorApp::DeviceGlyph(device.platform) : hstring(),
                main ? device.name : hstring(), subtitle, !online, connected);
            screen.HorizontalAlignment(HorizontalAlignment::Left);
            screen.VerticalAlignment(VerticalAlignment::Top);
            screen.Margin(ThicknessHelper::FromLengths((display.x - device.bounds.x) * scale,
                (display.y - device.bounds.y) * scale, 0, 0));
            group.Children().Append(screen);
        }

        if (!ghost)
        {
            auto weak = get_weak();
            auto peerId = device.peerId;
            group.PointerPressed([weak, peerId](IInspectable const& sender, PointerRoutedEventArgs const& args)
            {
                auto self = weak.get();
                if (!self || self->m_drag) return;
                if (auto device = self->FindLayoutDevice(peerId))
                {
                    self->BeginDrag(*device, sender.as<FrameworkElement>(), args);
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
                if (self && self->m_drag && !self->m_drag->fromChip)
                {
                    self->m_drag.reset();
                    self->RenderArrangement();
                }
            });
        }
        return group;
    }

    void MainWindow::BeginDrag(LayoutDevice const& device, FrameworkElement const& element,
        PointerRoutedEventArgs const& args)
    {
        auto position = args.GetCurrentPoint(ArrangementCanvas()).Position();
        auto left = Canvas::GetLeft(element);
        auto top = Canvas::GetTop(element);
        m_drag = LayoutDrag{ device.peerId, element,
            Point{ static_cast<float>(position.X - left), static_cast<float>(position.Y - top) }, position, false };
        element.CapturePointer(args.Pointer());
        // Drawn above the other devices while it moves.
        Canvas::SetZIndex(element, 10);
        args.Handled(true);
    }

    void MainWindow::MoveDrag(PointerRoutedEventArgs const& args)
    {
        if (!m_drag) return;
        auto position = args.GetCurrentPoint(ArrangementCanvas()).Position();
        if (Distance(position, m_drag->start) >= DragThreshold)
        {
            m_drag->element.Visibility(Visibility::Visible);
        }
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
        bool moved = Distance(position, drag.start) >= DragThreshold;
        bool inside = position.X >= 0 && position.Y >= 0
            && position.X <= ArrangementHost().ActualWidth() && position.Y <= ArrangementHost().ActualHeight();
        auto device = FindLayoutDevice(drag.peerId);
        if (moved && inside && device)
        {
            // Propose the drop position; the core snaps it and the next
            // snapshot draws where it actually landed.
            double worldX = Canvas::GetLeft(drag.element) / m_layoutScale + m_layoutOriginX;
            double worldY = Canvas::GetTop(drag.element) / m_layoutScale + m_layoutOriginY;
            m_agent->SetPlacement(drag.peerId, worldX - device->bounds.x, worldY - device->bounds.y);
        }
        else if (moved && !drag.fromChip)
        {
            m_agent->RemovePlacement(drag.peerId);
        }

        // Redraw from the stored arrangement shortly, in case the drop changed
        // nothing (a click, or dropped back on its spot) and no snapshot comes.
        m_layoutSignature.clear();
        auto timer = DispatcherQueue().CreateTimer();
        timer.Interval(std::chrono::milliseconds(moved ? 250 : 0));
        timer.IsRepeating(false);
        auto weak = get_weak();
        timer.Tick([weak](auto&& sender, auto&&)
        {
            sender.Stop();
            if (auto self = weak.get(); self && !self->m_drag) self->RenderArrangement();
        });
        timer.Start();
    }
}
