using CellGlide.App.Hosting;
using CellGlide.App.Shell;
using CellGlide.Core.Models;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace CellGlide.App.Controls;

/// <summary>
/// The phone at a glance, laid out as cg-gui's widgets/phone_card.rs: carrier, radio, model and
/// battery; speeds and the last minute's trail; quality, battery and heat. Every text is the DLL's
/// <see cref="PhoneCardView"/>. The full layout (overview) has bigger speeds, a wider trail and rules
/// between the blocks; the compact one (panel) does not. A dimmed card is drawn at half strength.
/// </summary>
public sealed partial class PhoneCard : UserControl
{
    public static readonly DependencyProperty ViewProperty = DependencyProperty.Register(
        nameof(View), typeof(object), typeof(PhoneCard), new PropertyMetadata(null, (d, _) => ((PhoneCard)d).Apply()));

    public static readonly DependencyProperty CompactProperty = DependencyProperty.Register(
        nameof(Compact), typeof(bool), typeof(PhoneCard), new PropertyMetadata(false, (d, _) => ((PhoneCard)d).Apply()));

    readonly Brush _secondary;

    public PhoneCard()
    {
        InitializeComponent();
        _secondary = BatteryText.Foreground;
        Apply();
        // The tone brushes follow a switch to or from high contrast while the card is shown.
        Loaded += (_, _) => Show.Changed += Apply;
        Unloaded += (_, _) => Show.Changed -= Apply;
    }

    public PhoneCardView? View
    {
        get => (PhoneCardView?)GetValue(ViewProperty);
        set => SetValue(ViewProperty, value);
    }

    /// <summary>The panel's layout rather than the overview's.</summary>
    public bool Compact
    {
        get => (bool)GetValue(CompactProperty);
        set => SetValue(CompactProperty, value);
    }

    void Apply()
    {
        if (View is not { } card)
        {
            Visibility = Visibility.Collapsed;
            Trail.Runs = null;
            return;
        }
        Visibility = Visibility.Visible;
        var full = !Compact;
        Opacity = Show.Opacity(card.Dim);

        TitleText.Text = card.Title;
        RadioText.Text = card.Radio ?? "";
        RadioTag.Visibility = Show.Visible(card.Radio);
        ModelText.Text = card.Model ?? "";
        ModelText.Visibility = Show.Visible(card.Model);
        BatteryText.Text = card.Battery?.Label ?? "";
        BatteryText.Visibility = Show.Visible(card.Battery);
        BatteryText.Foreground = card.Battery?.LowPower == true ? Show.LowPowerBrush() : _secondary;

        NotSharingText.Text = L.T("phone.not_sharing");
        NotSharingText.Visibility = Show.Visible(card.Speeds == Speeds.NotShared);

        SpeedRow.Visibility = Show.Visible(card.Speeds == Speeds.Shared);
        // A reading the phone did not send leaves its arrow out (the arrows are symbols, as in cg-gui).
        SetSpeed(DownText, "↓ ", card.Download, Tone.Blue, full);
        SetSpeed(UpText, "↑ ", card.Upload, Tone.Purple, full);
        Trail.Width = full ? 200 : 120;
        Trail.Height = full ? 40 : 24;
        Trail.Runs = card.Speeds == Speeds.Shared ? card.Trail : null;

        QualityText.Text = card.Quality ?? "";
        QualityText.Visibility = Show.Visible(card.Quality);
        QualityRule.Visibility = Show.Visible(full && card.Quality is not null);
        BatteryLineText.Text = card.BatteryLine ?? "";
        BatteryLineText.Visibility = Show.Visible(card.BatteryLine);
        BatteryRule.Visibility = Show.Visible(full && card.BatteryLine is not null);

        HeatText.Text = card.Heat?.Text ?? "";
        HeatText.Visibility = Show.Visible(card.Heat);
        HeatText.Foreground = Show.Brush(card.Heat?.Severe == true ? Tone.Red : Tone.Orange);
    }

    static void SetSpeed(TextBlock block, string arrow, string? value, Tone tone, bool full)
    {
        block.Visibility = Show.Visible(value);
        block.Text = value is null ? "" : arrow + value;
        block.FontSize = full ? 20 : 13;
        block.FontWeight = full ? FontWeights.SemiBold : FontWeights.Normal;
        block.Foreground = Show.Brush(tone);
    }
}
