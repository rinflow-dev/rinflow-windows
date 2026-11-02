// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#pragma once

// The app's windows, for the windows themselves (App.xaml.cpp keeps them).
// Rinflow.exe runs once per user session: a second launch hands its
// request to the running one (Main.cpp), which shows the main window, or
// the tray panel for --tray. The app ends a moment after its last window
// closes.
namespace RinflowApp
{
    // The running app's message-only window, and the mutex it holds.
    inline constexpr wchar_t ActivationWindowClass[] = L"Rinflow.App.Activation";
    inline constexpr wchar_t InstanceMutexName[] = L"Local\\Rinflow.App";
    // WM_COPYDATA id of a request: "main" or "tray" (UTF-16, terminated).
    inline constexpr ULONG_PTR ActivationRequest = 0x4C43;

    // Main.cpp hands over the instance mutex before the app starts.
    void KeepInstanceMutex(HANDLE mutex);

    // Opens (or brings back) the main window, on the settings page if asked.
    void ShowMainWindow(bool settings);
    void TrayPanelClosed();
    // Closes every window and ends the app.
    void QuitApp();
    // Starts the app afresh on the settings page in `language` (every string
    // is loaded again), and ends this one. The agent keeps running.
    void RelaunchApp(winrt::hstring const& language);
    // RelaunchApp is under way: closing the main window must not quit the
    // agent even with 关闭窗口时保留在托盘 off.
    bool IsRelaunching();
}
