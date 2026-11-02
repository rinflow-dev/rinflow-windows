// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#pragma once

#include <commctrl.h>

namespace RinflowApp
{
    // The window icon appears on the taskbar and in Alt+Tab, which are drawn in
    // the Windows theme. That can differ from the app theme and from the
    // in-app theme setting, so it decides the variant here. Each variant is
    // close to invisible on the other theme's surfaces.
    inline bool SystemThemeIsDark()
    {
        DWORD light = 1;
        DWORD size = sizeof(light);
        RegGetValueW(HKEY_CURRENT_USER, L"Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
            L"SystemUsesLightTheme", RRF_RT_REG_DWORD, nullptr, &light, &size);
        return light == 0;
    }

    // SetIcon requires an ICO. Resolve it beside the executable, independently
    // of the working directory used by Explorer, startup or a shortcut.
    inline void SetWindowIcon(winrt::Microsoft::UI::Windowing::AppWindow const& window)
    {
        std::wstring path(32768, L'\0');
        auto length = GetModuleFileNameW(nullptr, path.data(), static_cast<DWORD>(path.size()));
        if (!length || length >= path.size()) winrt::throw_last_error();
        path.resize(length);
        path.resize(path.find_last_of(L"\\/") + 1);
        path += SystemThemeIsDark() ? L"Assets\\Rinflow.Dark.ico" : L"Assets\\Rinflow.ico";
        window.SetIcon(path);
    }

    // Projected WinRT types cannot be allocated directly.
    struct WindowIconOwner
    {
        winrt::Microsoft::UI::Windowing::AppWindow window;
    };

    inline LRESULT CALLBACK WindowIconProc(HWND hwnd, UINT message, WPARAM wparam, LPARAM lparam, UINT_PTR id, DWORD_PTR data)
    {
        auto owner = reinterpret_cast<WindowIconOwner*>(data);
        if (message == WM_SETTINGCHANGE && lparam
            && CompareStringOrdinal(reinterpret_cast<wchar_t const*>(lparam), -1, L"ImmersiveColorSet", -1, TRUE) == CSTR_EQUAL)
        {
            // A stale icon is not worth ending the app from a window procedure.
            try { SetWindowIcon(owner->window); } catch (...) {}
        }
        else if (message == WM_NCDESTROY)
        {
            RemoveWindowSubclass(hwnd, WindowIconProc, id);
            delete owner;
        }
        return DefSubclassProc(hwnd, message, wparam, lparam);
    }

    // Sets the icon and keeps it matching the Windows theme until the window
    // closes. XAML's ActualThemeChanged only reports the app's own theme.
    inline void KeepWindowIconThemed(HWND hwnd, winrt::Microsoft::UI::Windowing::AppWindow const& window)
    {
        SetWindowIcon(window);
        auto owned = std::make_unique<WindowIconOwner>(window);
        if (SetWindowSubclass(hwnd, WindowIconProc, 1, reinterpret_cast<DWORD_PTR>(owned.get())))
        {
            owned.release();
        }
    }
}
