using System.ComponentModel;
using CellGlide.App.Hosting;
using CellGlide.App.Shell;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;
using CommunityToolkit.Mvvm.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace CellGlide.App.Controls;

/// <summary>
/// The wireless connection under the connect button, on the overview and (<see cref="Compact"/>) the panel, from
/// <see cref="WirelessViewModel"/>:
/// <list type="bullet">
/// <item>offered: "Connect Wirelessly" (after a failure "Try Again", under the failure's title and reason);</item>
/// <item>opening the network and waiting for the iPhone: the steps card (network name, password with Copy, the
/// three steps and the note; the password comes with the info the view model asks for while the card is
/// shown) and Cancel;</item>
/// <item>pairing: "Check the Code", which opens the code dialog in the main window (<see cref="ShowCode"/>), and
/// Cancel;</item>
/// <item>connecting and reconnecting: Cancel, and while reconnecting the hint that the iPhone may have joined
/// another Wi‑Fi it knows;</item>
/// <item>connected: nothing here (the state's title is the wireless line; Disconnect ends it).</item>
/// </list>
/// The phase's title and sentence are the state's own (the overview and the panel show them above), so they
/// are not repeated here, except for a failure, whose state is the usual one.
/// </summary>
public sealed partial class WirelessCard : UserControl
{
    WirelessViewModel? _wireless;
    readonly CopyFeedback _copy;

    public WirelessCard()
    {
        _copy = new CopyFeedback(Render);
        InitializeComponent();
        Loaded += (_, _) =>
        {
            // Loaded can come twice without an Unloaded between: never two handlers.
            Show.Changed -= Render;
            Show.Changed += Render;
            Render();
        };
        Unloaded += (_, _) => Show.Changed -= Render;
        Show.AnnounceChanges(FailureDetail);
        Show.AnnounceChanges(HintText);
        Show.AnnounceChanges(ErrorLine);
        Render();
    }

    public static readonly DependencyProperty CompactProperty = DependencyProperty.Register(
        nameof(Compact), typeof(bool), typeof(WirelessCard), new PropertyMetadata(false, (d, _) => ((WirelessCard)d).Render()));

    /// <summary>The panel's sizes and stacked buttons rather than the overview's.</summary>
    public bool Compact
    {
        get => (bool)GetValue(CompactProperty);
        set => SetValue(CompactProperty, value);
    }

    /// <summary>"Check the Code": opens the code dialog (in the main window).</summary>
    public Action? ShowCode { get; set; }

    /// <summary>The wireless connection to show and act on (set once, by the page or the panel).</summary>
    public WirelessViewModel? Wireless
    {
        get => _wireless;
        set
        {
            if (_wireless is not null)
            {
                _wireless.PropertyChanged -= OnChanged;
            }
            _wireless = value;
            if (value is not null)
            {
                value.PropertyChanged += OnChanged;
                foreach (var command in new INotifyPropertyChanged[] { value.StartCommand, value.CancelCommand })
                {
                    command.PropertyChanged += (_, e) =>
                    {
                        if (e.PropertyName == nameof(IAsyncRelayCommand.IsRunning))
                        {
                            Render();
                        }
                    };
                }
            }
            Render();
        }
    }

    void OnChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(WirelessViewModel.View) or nameof(WirelessViewModel.Info) or nameof(WirelessViewModel.Error))
        {
            Render();
        }
    }

    void Render()
    {
        var view = _wireless?.View;
        var info = _wireless?.Info;
        var small = Compact ? 11 : 13;
        Visibility = Show.Visible(view is not null || _wireless?.Error is not null);

        // A failure: its title and reason (the state's own title is the usual one then).
        var failed = view?.Phase == WirelessPhase.Failed;
        FailureBox.Visibility = Show.Visible(failed);
        FailureTitle.Text = failed ? view!.Title : "";
        FailureTitle.FontSize = small;
        FailureDetail.Text = failed ? view!.Detail : "";
        FailureDetail.FontSize = small;
        FailureDetail.Foreground = Show.Brush(Tone.Red);

        // The steps card: what the info says, once it came; the network name from the state until then.
        StepsBox.Visibility = Show.Visible(view?.ShowSteps == true);
        var unavailable = info?.Unavailable;
        NetworkGrid.Visibility = Show.Visible(view?.ShowSteps == true && unavailable is null && (info is not null || view?.Ssid is not null));
        SsidLabel.Text = L.T("wireless.info.ssid");
        SsidText.Text = info?.Ssid ?? view?.Ssid ?? "";
        PassphraseLabel.Text = L.T("wireless.info.passphrase");
        PassphraseText.Text = info?.Passphrase ?? "";
        // The password row only once the info came (the state never has it).
        PassphraseLabel.Visibility = Show.Visible(info is not null);
        PassphraseText.Visibility = Show.Visible(info is not null);
        foreach (var text in new[] { SsidLabel, PassphraseLabel })
        {
            text.FontSize = small;
            text.Style = Show.ThemeStyle("SecondaryText");
        }
        SsidText.FontSize = small;
        PassphraseText.FontSize = small;
        CopyFeedback.SetLabel(CopyButton, info is not null && _copy.IsCopied(info.Passphrase));
        AutomationProperties.SetHelpText(CopyButton, L.T("wireless.info.passphrase"));
        CopyButton.Visibility = Show.Visible(info is not null);
        var steps = info?.Steps ?? [];
        if (StepsList.Children.Count != steps.Count
            || !StepsList.Children.OfType<TextBlock>().Select(t => t.Text).SequenceEqual(steps.Select((s, i) => $"{i + 1}. {s}")))
        {
            StepsList.Children.Clear();
            for (var i = 0; i < steps.Count; i++)
            {
                StepsList.Children.Add(new TextBlock { Text = $"{i + 1}. {steps[i]}", TextWrapping = TextWrapping.Wrap, FontSize = small });
            }
        }
        NoteText.Text = info?.Note ?? "";
        NoteText.FontSize = Compact ? 11 : 12;
        NoteText.Style = Show.ThemeStyle("SecondaryText");
        NoteText.Visibility = Show.Visible(info is not null && unavailable is null);
        UnavailableText.Text = unavailable ?? "";
        UnavailableText.FontSize = small;
        UnavailableText.Foreground = Show.Brush(Tone.Orange);
        UnavailableText.Visibility = Show.Visible(unavailable);

        // While reconnecting.
        HintText.Text = view?.Hint ?? "";
        HintText.FontSize = small;
        HintText.Foreground = Show.Brush(Tone.Orange);
        HintText.Visibility = Show.Visible(view?.Hint);

        ErrorLine.Text = _wireless?.Error ?? "";
        ErrorLine.FontSize = small;
        ErrorLine.Foreground = Show.Brush(Tone.Red);
        ErrorLine.Visibility = Show.Visible(_wireless?.Error);

        // The buttons.
        StartButton.Content = view?.ButtonLabel ?? "";
        StartButton.Visibility = Show.Visible(view is { Offered: true, ButtonLabel: not null });
        ShowCodeButton.Content = L.T("wireless.action.show_code");
        ShowCodeButton.Visibility = Show.Visible(view is { Phase: WirelessPhase.Pairing } && ShowCode is not null);
        CancelButton.Content = L.T("wireless.action.cancel");
        CancelButton.Visibility = Show.Visible(view?.CanCancel == true);
        foreach (var button in new[] { StartButton, ShowCodeButton, CancelButton })
        {
            button.HorizontalAlignment = Compact ? HorizontalAlignment.Stretch : HorizontalAlignment.Left;
        }
        Buttons.Orientation = Compact ? Orientation.Vertical : Orientation.Horizontal;
        Buttons.Visibility = Show.Visible(StartButton.Visibility == Visibility.Visible
            || ShowCodeButton.Visibility == Visibility.Visible
            || CancelButton.Visibility == Visibility.Visible);
        // Off while their command runs; focus moves to the next button that is on.
        var starting = _wireless?.StartCommand.IsRunning == true;
        var cancelling = _wireless?.CancelCommand.IsRunning == true;
        Show.SetEnabled(
            [(StartButton, !starting), (CancelButton, !cancelling)],
            [StartButton, ShowCodeButton, CancelButton]);
    }

    async void OnStartClick(object sender, RoutedEventArgs e)
    {
        if (_wireless is { StartCommand: { IsRunning: false } start })
        {
            // A refusal leaves its sentence in Error, shown here.
            await start.ExecuteAsync(null);
        }
    }

    async void OnCancelClick(object sender, RoutedEventArgs e)
    {
        if (_wireless is { CancelCommand: { IsRunning: false } cancel })
        {
            await cancel.ExecuteAsync(null);
        }
    }

    void OnShowCodeClick(object sender, RoutedEventArgs e) => ShowCode?.Invoke();

    void OnCopyClick(object sender, RoutedEventArgs e)
    {
        if (_wireless?.Info is { } info)
        {
            _copy.Copy(info.Passphrase, CopyButton);
        }
    }
}
