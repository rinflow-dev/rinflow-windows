#pragma once

// The app's windows, for the windows themselves (App.xaml.cpp keeps them).
// LinkCursor.exe runs once per user session: a second launch hands its
// request to the running one (Main.cpp), which shows the main window, or
// the tray panel for --tray. The app ends a moment after its last window
// closes.
namespace LinkCursorApp
{
    // The running app's message-only window, and the mutex it holds.
    inline constexpr wchar_t ActivationWindowClass[] = L"LinkCursor.App.Activation";
    inline constexpr wchar_t InstanceMutexName[] = L"Local\\LinkCursor.App";
    // WM_COPYDATA id of a request: "main" or "tray" (UTF-16, terminated).
    inline constexpr ULONG_PTR ActivationRequest = 0x4C43;

    // Main.cpp hands over the instance mutex before the app starts.
    void KeepInstanceMutex(HANDLE mutex);

    // Opens (or brings back) the main window, on the settings page if asked.
    void ShowMainWindow(bool settings);
    void TrayPanelClosed();
    // Closes every window and ends the app.
    void QuitApp();
}
