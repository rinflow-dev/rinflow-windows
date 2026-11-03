// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#pragma once

// Text shown in code, in the user's language: Strings\<language>\Resources.resw,
// the same resources XAML takes through x:Uid. English is the default, for any
// language without its own file.
namespace RinflowApp
{
    // The string `key` (a name without a dot, e.g. "TransferCancel").
    winrt::hstring Text(std::wstring_view key);

    // `key` with {0}, {1}, … replaced by the arguments.
    template <typename... Args>
    winrt::hstring Format(std::wstring_view key, Args const&... args)
    {
        auto text = Text(key);
        try
        {
            return winrt::hstring(std::vformat(std::wstring_view(text), std::make_wformat_args(args...)));
        }
        catch (std::format_error const&)
        {
            // A translation with broken placeholders: shown as it is.
            return text;
        }
    }

    // The language the strings are in ("en-US", "zh-CN" or "ja-JP"). Windows set
    // it as their Language: Chinese and Japanese share characters drawn
    // differently, and text without it can get the other language's glyphs.
    winrt::hstring LanguageTag();

    // The `language` setting the agent keeps in settings.json ("system",
    // "en-US", "zh-CN" or "ja-JP"), applied before the first window loads: resources
    // are picked by it from then on. Read from the file because the agent's
    // snapshot only arrives later.
    void ApplyLanguageSetting();
}
