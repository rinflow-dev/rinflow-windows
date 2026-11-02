// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#include "pch.h"
#include "Strings.h"

#include <fstream>
#include <sstream>
#include <winrt/Microsoft.Windows.ApplicationModel.Resources.h>
#include <winrt/Microsoft.Windows.Globalization.h>

namespace LinkCursorApp
{
    void ApplyLanguageSetting()
    {
        std::wstring language;
        // Given by the app that started this one after the language changed.
        std::wstring_view commandLine = GetCommandLineW();
        constexpr std::wstring_view flag = L"--language=";
        if (auto at = commandLine.find(flag); at != std::wstring_view::npos)
        {
            auto value = commandLine.substr(at + flag.size());
            language = std::wstring(value.substr(0, value.find(L' ')));
        }
        else if (auto folder = wil::TryGetEnvironmentVariableW(L"LOCALAPPDATA"))
        {
            std::ifstream file(std::wstring(folder.get()) + L"\\LinkCursor\\settings.json", std::ios::binary);
            std::stringstream text;
            text << file.rdbuf();
            winrt::Windows::Data::Json::JsonObject settings{ nullptr };
            if (file && winrt::Windows::Data::Json::JsonObject::TryParse(winrt::to_hstring(text.str()), settings))
            {
                language = settings.GetNamedString(L"language", L"system");
            }
        }
        // Empty follows the user's Windows languages; anything else but
        // Chinese falls back to English (the default language).
        winrt::hstring chosen = language == L"zh-CN" || language == L"en-US" ? winrt::hstring(language) : winrt::hstring();
        try
        {
            winrt::Microsoft::Windows::Globalization::ApplicationLanguages::PrimaryLanguageOverride(chosen);
        }
        catch (winrt::hresult_error const&)
        {
        }
    }

    winrt::hstring Text(std::wstring_view key)
    {
        // MRT Core picks Strings\<language>\Resources.resw by the user's
        // languages, as x:Uid does.
        static winrt::Microsoft::Windows::ApplicationModel::Resources::ResourceLoader loader;
        try
        {
            auto text = loader.GetString(key);
            if (!text.empty()) return text;
        }
        catch (winrt::hresult_error const&)
        {
        }
        // A missing string shows its key rather than nothing.
        return winrt::hstring(key);
    }

    bool IsChinese()
    {
        static bool chinese = Text(L"LanguageTag") == L"zh-CN";
        return chinese;
    }
}
