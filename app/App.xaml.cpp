#include "pch.h"
#include "App.xaml.h"
#include "AppShell.h"
#include "MainWindow.xaml.h"
#include "TrayWindow.xaml.h"

// Fully qualified: WIL declares a global ::Microsoft namespace as well.
using namespace winrt;
using namespace winrt::Microsoft::UI::Xaml;

namespace
{
    winrt::LinkCursor::implementation::App* s_app = nullptr;
    wil::unique_mutex s_instanceMutex;

    // A tray click within this long after the panel closed was the click
    // that closed it: the panel stays closed (a toggle).
    constexpr auto TrayReopenGuard = std::chrono::milliseconds(600);
    // The app lingers this long after its last window, so a tray click that
    // closed the panel finds it running (and the guard above applies).
    constexpr auto ExitDelay = std::chrono::milliseconds(800);

    void BringToFront(Window const& window)
    {
        HWND hwnd{};
        if (FAILED(window.as<::IWindowNative>()->get_WindowHandle(&hwnd))) return;
        if (IsIconic(hwnd)) ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
    }

    // Requests from later launches (Main.cpp), on the UI thread.
    LRESULT CALLBACK ActivationProc(HWND window, UINT message, WPARAM wparam, LPARAM lparam)
    {
        if (message == WM_COPYDATA)
        {
            auto data = reinterpret_cast<COPYDATASTRUCT const*>(lparam);
            if (!s_app || !data || data->dwData != LinkCursorApp::ActivationRequest || data->cbData < sizeof(wchar_t))
            {
                return 0;
            }
            // Taken only by an app that stays: one about to exit says no, and
            // the launch waits to become the app itself.
            if (!s_app->AcceptRequest())
            {
                return 0;
            }
            std::wstring request(static_cast<wchar_t const*>(data->lpData), data->cbData / sizeof(wchar_t) - 1);
            // Handled after this message: the sender is waiting, and windows
            // are better not opened from inside a sent message.
            winrt::Microsoft::UI::Dispatching::DispatcherQueue::GetForCurrentThread().TryEnqueue([request]
            {
                if (s_app) s_app->Launch(request);
            });
            return 1;
        }
        return DefWindowProcW(window, message, wparam, lparam);
    }
}

namespace LinkCursorApp
{
    void KeepInstanceMutex(HANDLE mutex) { s_instanceMutex.reset(mutex); }
    void ShowMainWindow(bool settings) { if (s_app) s_app->ShowMain(settings); }
    void TrayPanelClosed() { if (s_app) s_app->TrayClosed(); }
    void QuitApp() { if (s_app) s_app->Quit(); }
}

namespace winrt::LinkCursor::implementation
{
    App::App()
    {
        // Xaml objects should not call InitializeComponent during construction.
        // See https://github.com/microsoft/cppwinrt/tree/master/nuget#initializecomponent
        s_app = this;

#if defined _DEBUG && !defined DISABLE_XAML_GENERATED_BREAK_ON_UNHANDLED_EXCEPTION
        UnhandledException([](IInspectable const&, UnhandledExceptionEventArgs const& e)
        {
            if (IsDebuggerPresent())
            {
                auto errorMessage = e.Message();
                __debugbreak();
            }
        });
#endif
    }

    void App::OnLaunched([[maybe_unused]] LaunchActivatedEventArgs const& e)
    {
        // The app decides when it ends (ExitWhenIdle), not XAML: closing the
        // tray panel must not end it while a request is on its way.
        DispatcherShutdownMode(Microsoft::UI::Xaml::DispatcherShutdownMode::OnExplicitShutdown);

        WNDCLASSW windowClass{};
        windowClass.lpfnWndProc = ActivationProc;
        windowClass.hInstance = GetModuleHandleW(nullptr);
        windowClass.lpszClassName = LinkCursorApp::ActivationWindowClass;
        RegisterClassW(&windowClass);
        m_activationWindow = CreateWindowExW(0, LinkCursorApp::ActivationWindowClass, L"", 0, 0, 0, 0, 0,
            HWND_MESSAGE, nullptr, windowClass.hInstance, nullptr);

        m_exitTimer = Microsoft::UI::Dispatching::DispatcherQueue::GetForCurrentThread().CreateTimer();
        m_exitTimer.Interval(ExitDelay);
        m_exitTimer.IsRepeating(false);
        m_exitTimer.Tick([](auto&&, auto&&)
        {
            if (s_app && !s_app->m_main && !s_app->m_tray) s_app->Shutdown();
        });

        std::wstring_view commandLine = GetCommandLineW();
        Launch(commandLine.find(L"--tray") != std::wstring_view::npos ? L"tray" : L"main");
    }

    bool App::AcceptRequest()
    {
        if (m_shuttingDown)
        {
            return false;
        }
        // The window this request opens comes after this message; the app
        // must not end in between.
        if (m_exitTimer) m_exitTimer.Stop();
        return true;
    }

    void App::Launch(std::wstring_view request)
    {
        if (m_shuttingDown)
        {
            return;
        }
        if (request == L"tray")
        {
            ShowTray();
        }
        else
        {
            ShowMain(false);
        }
        // The click that closed the panel opens nothing: end as usual.
        ExitWhenIdle();
    }

    void App::ShowMain(bool settings)
    {
        if (m_shuttingDown)
        {
            return;
        }
        m_exitTimer.Stop();
        if (!m_main)
        {
            m_main = make<MainWindow>();
            m_main.Closed([](auto&&, auto&&)
            {
                if (s_app) s_app->MainClosed();
            });
        }
        get_self<MainWindow>(m_main)->ShowPage(settings ? MainWindow::Page::Settings : MainWindow::Page::Devices);
        m_main.Activate();
        BringToFront(m_main);
    }

    void App::ShowTray()
    {
        if (m_tray)
        {
            get_self<TrayWindow>(m_tray)->Dismiss();
            return;
        }
        if (std::chrono::steady_clock::now() - m_trayClosedAt < TrayReopenGuard)
        {
            return;
        }
        m_exitTimer.Stop();
        m_tray = make<TrayWindow>();
        get_self<TrayWindow>(m_tray)->ShowNearPointer();
    }

    void App::MainClosed()
    {
        // Released once the window has finished closing, not from inside its
        // own Closed event.
        auto closing = std::move(m_main);
        Microsoft::UI::Dispatching::DispatcherQueue::GetForCurrentThread().TryEnqueue([closing] {});
        ExitWhenIdle();
    }

    void App::TrayClosed()
    {
        auto closing = std::move(m_tray);
        Microsoft::UI::Dispatching::DispatcherQueue::GetForCurrentThread().TryEnqueue([closing] {});
        m_trayClosedAt = std::chrono::steady_clock::now();
        ExitWhenIdle();
    }

    void App::ExitWhenIdle()
    {
        if (!m_main && !m_tray && m_exitTimer)
        {
            m_exitTimer.Stop();
            m_exitTimer.Start();
        }
    }

    void App::Quit()
    {
        // Out of the click that asked for it.
        Microsoft::UI::Dispatching::DispatcherQueue::GetForCurrentThread().TryEnqueue([]
        {
            if (!s_app) return;
            if (s_app->m_tray) s_app->m_tray.Close();
            if (s_app->m_main) s_app->m_main.Close();
            s_app->Shutdown();
        });
    }

    void App::Shutdown()
    {
        if (m_shuttingDown)
        {
            return;
        }
        m_shuttingDown = true;
        // A launch from now on waits for the mutex and becomes the app.
        if (m_activationWindow)
        {
            DestroyWindow(m_activationWindow);
            m_activationWindow = nullptr;
        }
        s_instanceMutex.reset();
        Exit();
    }
}
