#pragma once

// Text shown in code, in the user's language: Strings\<language>\Resources.resw,
// the same resources XAML takes through x:Uid. English is the default;
// Chinese (Simplified) is used when Windows runs in Chinese.
namespace LinkCursorApp
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

    // The language the strings are in: "zh-CN" or "en-US".
    bool IsChinese();

    // The `language` setting the agent keeps in settings.json ("system",
    // "zh-CN" or "en-US"), applied before the first window loads: resources
    // are picked by it from then on. Read from the file because the agent's
    // snapshot only arrives later.
    void ApplyLanguageSetting();
}
