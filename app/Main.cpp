// Entry point (replaces the XAML-generated one, DISABLE_XAML_GENERATED_MAIN):
// LinkCursor.exe runs once per user session. A second launch, such as the
// agent's tray icon starting `LinkCursor.exe --tray`, hands its request to
// the running app and exits.
//
// A named mutex says whether an app runs; its message-only window takes the
// request (WM_COPYDATA, with a timeout). An app that is shutting down
// destroys that window before it lets go of the mutex, so a launch in
// between waits a little and then becomes the app itself; nothing waits
// forever on an app that is gone.

#include "pch.h"
#include "App.xaml.h"
#include "AppShell.h"

namespace
{
    // How long a launch keeps trying to reach a running app, or to become it.
    constexpr auto Patience = std::chrono::seconds(15);
    constexpr DWORD RetryDelayMs = 100;
    constexpr UINT HandOverTimeoutMs = 3000;

    // False if no running app took the request.
    bool HandOver(std::wstring const& request)
    {
        HWND target = FindWindowExW(HWND_MESSAGE, nullptr, LinkCursorApp::ActivationWindowClass, nullptr);
        if (!target)
        {
            return false;
        }
        // This launch may bring a window to the front (it came from a
        // click); let the running app do so.
        DWORD process = 0;
        GetWindowThreadProcessId(target, &process);
        AllowSetForegroundWindow(process);
        COPYDATASTRUCT data{};
        data.dwData = LinkCursorApp::ActivationRequest;
        data.cbData = static_cast<DWORD>((request.size() + 1) * sizeof(wchar_t));
        data.lpData = const_cast<wchar_t*>(request.c_str());
        DWORD_PTR result = 0;
        return SendMessageTimeoutW(target, WM_COPYDATA, 0, reinterpret_cast<LPARAM>(&data),
                   SMTO_ABORTIFHUNG, HandOverTimeoutMs, &result) != 0
            && result == 1;
    }
}

int WINAPI wWinMain(HINSTANCE, HINSTANCE, PWSTR, int)
{
    std::wstring_view commandLine = GetCommandLineW();
    std::wstring request = commandLine.find(L"--tray") != std::wstring_view::npos ? L"tray" : L"main";

    auto deadline = std::chrono::steady_clock::now() + Patience;
    bool owner = false;
    while (!owner && std::chrono::steady_clock::now() < deadline)
    {
        wil::unique_mutex mutex{ CreateMutexW(nullptr, FALSE, LinkCursorApp::InstanceMutexName) };
        if (mutex && GetLastError() != ERROR_ALREADY_EXISTS)
        {
            LinkCursorApp::KeepInstanceMutex(mutex.release());
            owner = true;
            break;
        }
        mutex.reset();
        if (HandOver(request))
        {
            return 0;
        }
        // Starting up (no window yet) or shutting down: try again.
        Sleep(RetryDelayMs);
    }
    // A running app that never answered: never a second one beside it (two
    // would both talk to the agent and fight over the windows).
    if (!owner)
    {
        return 0;
    }

    winrt::init_apartment(winrt::apartment_type::single_threaded);
    ::winrt::Microsoft::UI::Xaml::Application::Start([](auto&&)
    {
        ::winrt::make<::winrt::LinkCursor::implementation::App>();
    });
    return 0;
}
