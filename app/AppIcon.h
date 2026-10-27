#pragma once

namespace LinkCursorApp
{
    // SetIcon requires an ICO. Resolve it beside the executable, independently
    // of the working directory used by Explorer, startup or a shortcut.
    inline void SetWindowIcon(winrt::Microsoft::UI::Windowing::AppWindow const& window, bool dark)
    {
        std::wstring path(32768, L'\0');
        auto length = GetModuleFileNameW(nullptr, path.data(), static_cast<DWORD>(path.size()));
        if (!length || length >= path.size()) winrt::throw_last_error();
        path.resize(length);
        path.resize(path.find_last_of(L"\\/") + 1);
        path += dark ? L"Assets\\LinkCursor.Dark.ico" : L"Assets\\LinkCursor.ico";
        window.SetIcon(path);
    }
}
