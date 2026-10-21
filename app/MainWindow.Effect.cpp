// The edge animation page, for tuning the push-through effect. Development
// builds only: the button that opens it is hidden in release builds, where
// the animation is going away.

#include "pch.h"
#include "MainWindow.xaml.h"

// Fully qualified: WIL declares a global ::Microsoft namespace as well.
using namespace winrt;
using namespace winrt::Microsoft::UI::Xaml;
using namespace winrt::Microsoft::UI::Xaml::Controls;
using namespace winrt::Windows::Data::Json;

namespace winrt::LinkCursor::implementation
{
    void MainWindow::SetupEffectPage()
    {
        auto number = [](std::wstring_view unit, double multiplier = 1.0, int decimals = 0)
        {
            return [unit, multiplier, decimals](double value)
            {
                auto text = decimals == 0
                    ? std::format(L"{:.0f}", value * multiplier)
                    : std::format(L"{:.2f}", value * multiplier);
                return unit.empty() ? text : text + L" " + std::wstring(unit);
            };
        };
        // Keys and units match EffectParams in agent/src/effect.rs.
        m_effectSliders = {
            { L"push_distance", PushDistanceSlider(), PushDistanceValue(), number(L"") },
            { L"release_delay", ReleaseDelaySlider(), ReleaseDelayValue(), number(L"毫秒", 1000) },
            { L"warm_seconds", WarmSecondsSlider(), WarmSecondsValue(), number(L"秒") },
            { L"corner_dead_zone", CornerDeadZoneSlider(), CornerDeadZoneValue(), number(L"pt") },
            { L"spring_response", SpringResponseSlider(), SpringResponseValue(), number(L"秒", 1, 2) },
            { L"spring_damping", SpringDampingSlider(), SpringDampingValue(), number(L"", 1, 2) },
            { L"bump_depth", BumpDepthSlider(), BumpDepthValue(), number(L"pt") },
            { L"bump_width", BumpWidthSlider(), BumpWidthValue(), number(L"pt") },
            { L"dot_size", DotSizeSlider(), DotSizeValue(), number(L"pt") },
            { L"opacity", OpacitySlider(), OpacityValue(), number(L"%", 100) },
        };

        auto weak = get_weak();
        auto edited = [weak](auto&&, auto&&)
        {
            if (auto self = weak.get()) self->OnEffectEdited();
        };
        for (auto const& entry : m_effectSliders)
        {
            entry.slider.ValueChanged(edited);
        }
        RippleToggle().Toggled(edited);
        EffectThemeCombo().SelectionChanged(edited);

        DemoToggle().Toggled([weak](auto&&, auto&&)
        {
            auto self = weak.get();
            if (self && !self->m_applyingEffect)
            {
                self->m_agent->SetEffectDemo(self->DemoToggle().IsOn());
            }
        });
        PreviewButton().Click([weak](auto&&, auto&&)
        {
            if (auto self = weak.get()) self->m_agent->PreviewEffect();
        });
        ResetEffectButton().Click([weak](auto&&, auto&&)
        {
            if (auto self = weak.get()) self->m_agent->ResetEffect();
        });
        UpdateEffectLabels();
    }

    void MainWindow::OnEffectEdited()
    {
        UpdateEffectLabels();
        if (m_applyingEffect)
        {
            return;
        }
        JsonObject params;
        for (auto const& entry : m_effectSliders)
        {
            params.Insert(entry.key, JsonValue::CreateNumberValue(entry.slider.Value()));
        }
        params.Insert(L"ripple", JsonValue::CreateBooleanValue(RippleToggle().IsOn()));
        hstring theme = L"auto";
        if (auto item = EffectThemeCombo().SelectedItem().try_as<ComboBoxItem>())
        {
            theme = unbox_value_or<hstring>(item.Tag(), L"auto");
        }
        params.Insert(L"theme", JsonValue::CreateStringValue(theme));
        m_agent->SetEffect(params);
    }

    void MainWindow::UpdateEffectLabels()
    {
        for (auto const& entry : m_effectSliders)
        {
            entry.label.Text(entry.format(entry.slider.Value()));
        }
    }

    void MainWindow::ApplyEffect(JsonObject const& state)
    {
        m_applyingEffect = true;

        // Values are copied only when the agent reloads them (start-up,
        // reset). Copying every snapshot would fight the slider being dragged.
        auto revision = state.GetNamedNumber(L"effect_revision", 0);
        if (revision != m_effectRevision)
        {
            m_effectRevision = revision;
            auto params = state.GetNamedObject(L"effect", JsonObject{});
            for (auto const& entry : m_effectSliders)
            {
                entry.slider.Value(params.GetNamedNumber(entry.key, entry.slider.Value()));
            }
            RippleToggle().IsOn(params.GetNamedBoolean(L"ripple", true));
            auto theme = params.GetNamedString(L"theme", L"auto");
            EffectThemeCombo().SelectedIndex(theme == L"light" ? 1 : theme == L"dark" ? 2 : 0);
            UpdateEffectLabels();
        }

        bool demo = state.GetNamedBoolean(L"effect_demo", false);
        if (DemoToggle().IsOn() != demo)
        {
            DemoToggle().IsOn(demo);
        }
        auto side = state.GetNamedString(L"demo_side", L"local");
        DemoSideText().Text(side == L"remote"
            ? L"现在在“另一台设备”上 · 往回推回到本机"
            : L"现在在本机 · 推向放置了设备的边缘进入另一台设备");
        DemoSideText().Visibility(demo ? Visibility::Visible : Visibility::Collapsed);

        m_applyingEffect = false;
    }
}
