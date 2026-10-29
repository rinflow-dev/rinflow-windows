// The tray panel: opened by clicking the agent's tray icon (the agent starts
// LinkCursor.exe --tray, which a running app takes over). A small borderless
// window by the notification area that closes as soon as it loses focus.

#include "pch.h"
#include "TrayWindow.xaml.h"
#if __has_include("TrayWindow.g.cpp")
#include "TrayWindow.g.cpp"
#endif
#include "AppShell.h"
#include "DeviceModel.h"

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
    // Rows beyond this many devices are left out; the main window has all.
    constexpr size_t MaxRows = 8;
}

namespace winrt::LinkCursor::implementation
{
    void TrayWindow::InitializeComponent()
    {
        TrayWindowT::InitializeComponent();

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
        m_agent = std::make_unique<::LinkCursorApp::AgentClient>();
        m_agent->Start(
            [dispatcher, weak](JsonObject const& message)
            {
                dispatcher.TryEnqueue([weak, message]
                {
                    // Not once the panel is closing: its window may be gone.
                    auto self = weak.get();
                    if (self && !self->m_closing && message.GetNamedString(L"type", L"") == L"state") self->ApplyState(message);
                });
            },
            [](bool) {});

        SharingToggle().Toggled([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (self && !self->m_applying)
            {
                self->m_agent->UpdateSetting(L"sharing", JsonValue::CreateBooleanValue(self->SharingToggle().IsOn()));
            }
        });
        // The main window opens after this click, then the panel closes.
        auto openMain = [weak](bool settings)
        {
            auto self = weak.get();
            if (!self) return;
            self->DispatcherQueue().TryEnqueue([settings] { LinkCursorApp::ShowMainWindow(settings); });
            self->Dismiss();
        };
        OpenButton().Click([openMain](auto&&, auto&&) { openMain(false); });
        SettingsButton().Click([openMain](auto&&, auto&&) { openMain(true); });
        QuitButton().Click([weak](auto&&, auto&&)
        {
            if (auto self = weak.get()) self->m_agent->Quit();
            LinkCursorApp::QuitApp();
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
            LinkCursorApp::TrayPanelClosed();
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
        FitToContent();
        Activate();
        HWND hwnd{};
        check_hresult(m_inner.as<::IWindowNative>()->get_WindowHandle(&hwnd));
        SetForegroundWindow(hwnd);
    }

    void TrayWindow::FitToContent()
    {
        auto monitor = MonitorFromPoint(m_anchor, MONITOR_DEFAULTTONEAREST);
        MONITORINFO info{ sizeof(info) };
        GetMonitorInfoW(monitor, &info);
        UINT dpiX = 96, dpiY = 96;
        if (FAILED(GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &dpiX, &dpiY))) dpiX = 96;
        m_scale = dpiX / 96.0;

        Root().Measure({ static_cast<float>(PanelWidth), std::numeric_limits<float>::infinity() });
        double height = (std::max)(static_cast<double>(Root().DesiredSize().Height), 200.0);
        int w = static_cast<int>(std::lround(PanelWidth * m_scale));
        int h = static_cast<int>(std::lround(height * m_scale));
        int gap = static_cast<int>(std::lround(Gap * m_scale));

        auto const& work = info.rcWork;
        auto const& full = info.rcMonitor;
        auto clamp = [](long value, long low, long high) { return (std::max)(low, (std::min)(value, high)); };
        long x, y;
        if (work.top > full.top)
        {
            // Taskbar at the top.
            x = clamp(m_anchor.x - w / 2, work.left + gap, work.right - w - gap);
            y = work.top + gap;
        }
        else if (work.left > full.left)
        {
            x = work.left + gap;
            y = clamp(m_anchor.y - h / 2, work.top + gap, work.bottom - h - gap);
        }
        else if (work.right < full.right)
        {
            x = work.right - w - gap;
            y = clamp(m_anchor.y - h / 2, work.top + gap, work.bottom - h - gap);
        }
        else
        {
            // Taskbar at the bottom (or hidden): rise from it.
            x = clamp(m_anchor.x - w / 2, work.left + gap, work.right - w - gap);
            y = work.bottom - h - gap;
        }
        AppWindow().MoveAndResize({ static_cast<int32_t>(x), static_cast<int32_t>(y), w, h });
    }

    void TrayWindow::ApplyState(JsonObject const& state)
    {
        // The user quit LinkCursor (the agent's tray): every window goes too.
        if (state.GetNamedBoolean(L"quitting", false))
        {
            LinkCursorApp::QuitApp();
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
        bool sharing = settings.GetNamedBoolean(L"sharing", true);
        if (SharingToggle().IsOn() != sharing) SharingToggle().IsOn(sharing);
        m_applying = false;

        auto model = LinkCursorApp::DeviceModel::FromState(state);
        auto before = m_devices.Size();
        m_devices.Clear();
        m_devices.Append(LinkCursorApp::LocalRow(model.localName));
        for (size_t i = 0; i < model.mine.size() && i < MaxRows; ++i)
        {
            m_devices.Append(LinkCursorApp::DeviceRow(model.mine[i]));
        }
        if (m_devices.Size() != before)
        {
            FitToContent();
        }
    }
}
