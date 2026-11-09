// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#include "pch.h"
#include "Strings.h"

#include <fstream>
#include <sstream>
#include <winrt/Microsoft.Windows.ApplicationModel.Resources.h>
#include <winrt/Microsoft.Windows.Globalization.h>

namespace
{
    using winrt::Windows::Data::Json::JsonObject;
    using winrt::Windows::Data::Json::JsonValueType;

    // The JSON object in a file; nullptr when there is none or it is not one.
    JsonObject ReadJsonObject(std::wstring const& path)
    {
        std::ifstream file(path, std::ios::binary);
        if (!file) return nullptr;
        std::stringstream text;
        text << file.rdbuf();
        JsonObject object{ nullptr };
        if (!JsonObject::TryParse(winrt::to_hstring(text.str()), object)) return nullptr;
        return object;
    }

    // `object[key]` if it is a string; empty otherwise (missing, null, or
    // another type, which GetNamedString would throw on).
    std::wstring StringOf(JsonObject const& object, std::wstring_view key)
    {
        if (!object || !object.HasKey(key)) return {};
        auto value = object.GetNamedValue(key);
        return value.ValueType() == JsonValueType::String ? std::wstring(value.GetString()) : std::wstring();
    }

    // The `language` setting the agent keeps: the `settings` section of
    // config.json (agent/src/config.rs). Before the agent's first start
    // after an update from 0.0.1, it is still in settings.json.
    std::wstring SavedLanguage(std::wstring const& folder)
    {
        if (auto config = ReadJsonObject(folder + L"\\config.json"))
        {
            auto settings = config.GetNamedValue(L"settings", nullptr);
            if (settings && settings.ValueType() == JsonValueType::Object)
            {
                return StringOf(settings.GetObject(), L"language");
            }
            return {};
        }
        return StringOf(ReadJsonObject(folder + L"\\settings.json"), L"language");
    }
}

namespace RinflowApp
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
            language = SavedLanguage(std::wstring(folder.get()) + L"\\Rinflow");
        }
        // Empty follows the user's Windows languages: the first one with
        // resources, then English (the default language).
        winrt::hstring chosen = language == L"en-US" || language == L"zh-CN" || language == L"ja-JP" ? winrt::hstring(language) : winrt::hstring();
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

    winrt::hstring LanguageTag()
    {
        return Text(L"LanguageTag");
    }
}
