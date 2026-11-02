// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#pragma once

#include "App.xaml.g.h"
#include <winrt/Rinflow.h>

namespace winrt::Rinflow::implementation
{
    struct App : AppT<App>
    {
        App();

        void OnLaunched(Microsoft::UI::Xaml::LaunchActivatedEventArgs const&);

        // A request, from this launch or handed over by a later one
        // (Main.cpp): "main" or "tray".
        void Launch(std::wstring_view request);
        // A later launch hands over a request: false while shutting down.
        // Otherwise the app stays until the request has opened its window.
        bool AcceptRequest();

        // See AppShell.h.
        void ShowMain(bool settings);
        void ShowTray();
        void TrayClosed();
        void Quit();
        void Relaunch(hstring const& language);

    private:
        void MainClosed();
        // Ends the app shortly unless a window opens meanwhile.
        void ExitWhenIdle();
        // Stops taking requests, lets go of the instance and exits.
        void Shutdown();

        winrt::Rinflow::MainWindow m_main{ nullptr };
        winrt::Rinflow::TrayWindow m_tray{ nullptr };
        Microsoft::UI::Dispatching::DispatcherQueueTimer m_exitTimer{ nullptr };
        HWND m_activationWindow{ nullptr };
        // When the tray panel last closed: a click on the tray icon closes
        // the panel (it loses focus) and then asks for it again.
        std::chrono::steady_clock::time_point m_trayClosedAt{};
        bool m_shuttingDown{ false };
    };
}
