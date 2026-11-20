// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// The tray panel: opened by clicking the agent's tray icon (the agent starts
// Rinflow.exe --tray, which a running app takes over). A small borderless
// window by the notification area that closes as soon as it loses focus.

#include "pch.h"
#include "TrayWindow.xaml.h"
#if __has_include("TrayWindow.g.cpp")
#include "TrayWindow.g.cpp"
#endif
#include "AppShell.h"
#include "DeviceModel.h"
#include "Strings.h"

// Fully qualified: WIL declares a global ::Microsoft namespace as well.
using namespace winrt;
using namespace winrt::Microsoft::UI::Xaml;
using namespace winrt::Microsoft::UI::Xaml::Controls;
using namespace winrt::Microsoft::UI::Windowing;
using namespace winrt::Windows::Data::Json;

namespace
{
    constexpr double PanelWidth = 300;
    constexpr double Gap = 10;
    // Devices driving this PC beyond this many are left out.
    constexpr size_t MaxRows = 8;

    // The taskbar on this monitor as it is drawn now, in physical pixels,
    // cut to the monitor. The work area only leaves out a taskbar that
    // always shows: one that hides itself, or the taller one of a tablet,
    // lies over it.
    std::optional<RECT> TaskbarOn(HMONITOR monitor, RECT const& screen)
    {
        for (auto className : { L"Shell_TrayWnd", L"Shell_SecondaryTrayWnd" })
        {
            HWND bar = nullptr;
            while ((bar = FindWindowExW(nullptr, bar, className, nullptr)) != nullptr)
            {
                RECT rect{}, shown{};
                if (IsWindowVisible(bar)
                    && MonitorFromWindow(bar, MONITOR_DEFAULTTONULL) == monitor
                    && GetWindowRect(bar, &rect)
                    && IntersectRect(&shown, &rect, &screen))
                {
                    return shown;
                }
            }
        }
        return std::nullopt;
    }
}

namespace winrt::Rinflow::implementation
{
    void TrayWindow::InitializeComponent()
    {
        TrayWindowT::InitializeComponent();
        Root().Language(RinflowApp::LanguageTag());

        auto presenter = OverlappedPresenter::Create();
        presenter.SetBorderAndTitleBar(false, false);
        presenter.IsResizable(false);
        presenter.IsMaximizable(false);
        presenter.IsMinimizable(false);
        presenter.IsAlwaysOnTop(true);
        AppWindow().SetPresenter(presenter);
        AppWindow().IsShownInSwitchers(false);

        HWND hwnd{};
        check_hresult(m_inner.as<::IWindowNative>()->get_WindowHandle(&hwnd));
        DWM_WINDOW_CORNER_PREFERENCE corner = DWMWCP_ROUND;
        DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner, sizeof(corner));

        auto dispatcher = DispatcherQueue();
        auto weak = get_weak();
        m_agent = std::make_unique<::RinflowApp::AgentClient>();
        m_agent->Start(
            [dispatcher, weak](JsonObject const& message)
            {
                dispatcher.TryEnqueue([weak, message]
                {
                    // Not once the panel is closing: its window may be gone.
                    auto self = weak.get();
                    if (!self || self->m_closing) return;
                    // As in MainWindow::ApplyMessage: a value of an
                    // unexpected type skips this snapshot, not the app.
                    try
                    {
                        if (message.GetNamedString(L"type", L"") == L"state") self->ApplyState(message);
                    }
                    catch (hresult_error const& error)
                    {
                        OutputDebugStringW((L"Rinflow: skipped an agent message: " + error.message() + L"\n").c_str());
                    }
                });
            },
            [](bool) {});

        LockToggle().Toggled([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (self && !self->m_applying)
            {
                self->m_agent->SetLocked(self->LockToggle().IsOn());
            }
        });
        // The main window opens after this click, then the panel closes.
        auto openMain = [weak](bool settings)
        {
            auto self = weak.get();
            if (!self) return;
            self->DispatcherQueue().TryEnqueue([settings] { RinflowApp::ShowMainWindow(settings); });
            self->Dismiss();
        };
        OpenButton().Click([openMain](auto&&, auto&&) { openMain(false); });
        SettingsButton().Click([openMain](auto&&, auto&&) { openMain(true); });
        QuitButton().Click([weak](auto&&, auto&&)
        {
            if (auto self = weak.get()) self->m_agent->Quit();
            RinflowApp::QuitApp();
        });

        // Like any flyout: gone once something else is clicked, or on Esc.
        Activated([weak](auto&&, WindowActivatedEventArgs const& args)
        {
            auto self = weak.get();
            if (!self) return;
            if (args.WindowActivationState() == WindowActivationState::Deactivated)
            {
                if (self->m_wasActive) self->Dismiss();
            }
            else
            {
                self->m_wasActive = true;
            }
        });
        Root().Loaded([weak](auto&&, auto&&)
        {
            if (auto self = weak.get(); self && !self->m_closing) self->FitToContent();
        });
        Root().KeyDown([weak](auto&&, Input::KeyRoutedEventArgs const& args)
        {
            if (args.Key() == Windows::System::VirtualKey::Escape)
            {
                if (auto self = weak.get()) self->Dismiss();
            }
        });
        Closed([weak](auto&&, auto&&)
        {
            if (auto self = weak.get())
            {
                self->m_closing = true;
                self->m_agent->Stop();
            }
            RinflowApp::TrayPanelClosed();
        });
    }

    void TrayWindow::Dismiss()
    {
        if (m_closing)
        {
            return;
        }
        m_closing = true;
        DispatcherQueue().TryEnqueue([strong = get_strong()]
        {
            strong->Close();
        });
    }

    void TrayWindow::ShowNearPointer()
    {
        GetCursorPos(&m_anchor);
        FindPlace();
        FitToContent();
        Activate();
        HWND hwnd{};
        check_hresult(m_inner.as<::IWindowNative>()->get_WindowHandle(&hwnd));
        SetForegroundWindow(hwnd);
    }

    void TrayWindow::FindPlace()
    {
        auto monitor = MonitorFromPoint(m_anchor, MONITOR_DEFAULTTONEAREST);
        MONITORINFO info{ sizeof(info) };
        GetMonitorInfoW(monitor, &info);
        auto const& full = info.rcMonitor;
        m_area = info.rcWork;

        // Once, as the panel opens: a tablet's taskbar may shrink back as
        // soon as the panel takes the focus, and the panel stays put.
        if (auto bar = TaskbarOn(monitor, full))
        {
            if (bar->right - bar->left >= bar->bottom - bar->top)
            {
                m_edge = bar->top - full.top < full.bottom - bar->bottom ? Edge::Top : Edge::Bottom;
            }
            else
            {
                m_edge = bar->left - full.left < full.right - bar->right ? Edge::Left : Edge::Right;
            }
            switch (m_edge)
            {
            case Edge::Top: m_area.top = (std::max)(m_area.top, bar->bottom); break;
            case Edge::Bottom: m_area.bottom = (std::min)(m_area.bottom, bar->top); break;
            case Edge::Left: m_area.left = (std::max)(m_area.left, bar->right); break;
            case Edge::Right: m_area.right = (std::min)(m_area.right, bar->left); break;
            }
        }
        else if (m_area.top > full.top) m_edge = Edge::Top;
        else if (m_area.left > full.left) m_edge = Edge::Left;
        else if (m_area.right < full.right) m_edge = Edge::Right;
        else m_edge = Edge::Bottom;
    }

    void TrayWindow::FitToContent()
    {
        auto monitor = MonitorFromPoint(m_anchor, MONITOR_DEFAULTTONEAREST);
        UINT dpiX = 96, dpiY = 96;
        if (FAILED(GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &dpiX, &dpiY))) dpiX = 96;
        m_scale = dpiX / 96.0;

        Root().Measure({ static_cast<float>(PanelWidth), std::numeric_limits<float>::infinity() });
        // Exactly as tall as the content. Before the first layout pass the
        // content measures nothing: a stand-in until Loaded fits it again.
        double height = Root().DesiredSize().Height;
        if (height < 1) height = 200;
        int w = static_cast<int>(std::lround(PanelWidth * m_scale));
        int h = static_cast<int>(std::lround(height * m_scale));
        int gap = static_cast<int>(std::lround(Gap * m_scale));

        // Beside the taskbar, never on it, and by the pointer along it.
        auto const& area = m_area;
        auto clamp = [](long value, long low, long high) { return (std::max)(low, (std::min)(value, high)); };
        long x = clamp(m_anchor.x - w / 2, area.left + gap, area.right - w - gap);
        long y = clamp(m_anchor.y - h / 2, area.top + gap, area.bottom - h - gap);
        switch (m_edge)
        {
        case Edge::Top: y = area.top + gap; break;
        case Edge::Bottom: y = area.bottom - h - gap; break;
        case Edge::Left: x = area.left + gap; break;
        case Edge::Right: x = area.right - w - gap; break;
        }
        AppWindow().MoveAndResize({ static_cast<int32_t>(x), static_cast<int32_t>(y), w, h });
    }

    void TrayWindow::ApplyState(JsonObject const& state)
    {
        // The user quit Rinflow (the agent's tray): every window goes too.
        if (state.GetNamedBoolean(L"quitting", false))
        {
            RinflowApp::QuitApp();
            return;
        }
        auto settings = state.GetNamedObject(L"settings", JsonObject{});
        auto theme = settings.GetNamedString(L"theme", L"system");
        if (theme != m_theme)
        {
            m_theme = theme;
            Root().RequestedTheme(theme == L"light" ? ElementTheme::Light
                : theme == L"dark" ? ElementTheme::Dark
                : ElementTheme::Default);
        }

        m_applying = true;
        bool locked = state.GetNamedBoolean(L"locked", false);
        if (LockToggle().IsOn() != locked) LockToggle().IsOn(locked);
        m_applying = false;

        // Only while another device drives this PC: which one. Otherwise
        // the panel has nothing to say about devices.
        auto before = m_devices.Size();
        m_devices.Clear();
        for (auto const& device : RinflowApp::DeviceModel::FromState(state).mine)
        {
            if (!device.connected || !device.controlling || m_devices.Size() >= MaxRows) continue;
            Rinflow::PeerItem row(device.id, device.name, L"", RinflowApp::DeviceGlyph(device.platform));
            if (device.latencyMs)
            {
                row.Latency(*device.latencyMs == 0 ? hstring(L"<1 ms") : hstring(std::format(L"{} ms", *device.latencyMs)));
            }
            m_devices.Append(row);
        }
        ControlledPanel().Visibility(m_devices.Size() > 0 ? Visibility::Visible : Visibility::Collapsed);
        if (m_devices.Size() != before)
        {
            FitToContent();
        }
    }
}
