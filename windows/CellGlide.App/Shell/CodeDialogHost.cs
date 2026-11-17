using CellGlide.App.Hosting;
using CellGlide.Core.ViewModels;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace CellGlide.App.Shell;

/// <summary>
/// The code dialog, in the main window only (one at a time; the panel shows "Check the Code", which opens the
/// window and this): "Confirm the code 384 271 on the iPhone" and "Compare it with the number on the iPhone's
/// screen", with "They Match" and "Cancel". It opens when a code comes while the window is on screen (or when
/// the window comes, or when asked), and closes by itself when the phase leaves pairing. "They Match" keeps it
/// open, showing the code with "Waiting for the iPhone to confirm too…" and only Cancel, until the iPhone
/// answers; Cancel ends the attempt; Esc only puts it away (<see cref="CodeDialogTracker"/> decides). No
/// default button: a code is compared, never confirmed by a reflexive Enter.
/// </summary>
public sealed class CodeDialogHost
{
    readonly WirelessViewModel _wireless;
    readonly Func<XamlRoot?> _root;
    readonly Func<bool> _onScreen;
    readonly CodeDialogTracker _tracker = new();
    ContentDialog? _dialog;
    TextBlock? _title;
    TextBlock? _detail;

    /// <param name="root">The main window's XAML root, once its content is loaded.</param>
    /// <param name="onScreen">Whether the main window is shown.</param>
    public CodeDialogHost(WirelessViewModel wireless, Func<XamlRoot?> root, Func<bool> onScreen)
    {
        _wireless = wireless;
        _root = root;
        _onScreen = onScreen;
        wireless.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName == nameof(WirelessViewModel.Code))
            {
                Follow();
            }
        };
    }

    /// <summary>"Check the Code": opens the dialog even after Esc put it away.</summary>
    public void Ask()
    {
        _tracker.Ask();
        Follow();
    }

    /// <summary>Looks again: a new code, the window shown, its content loaded.</summary>
    public void Follow()
    {
        var root = _root();
        switch (_tracker.Follow(_wireless.Code, _onScreen() && root is not null))
        {
            case CodeDialogStep.Open:
                _ = OpenAsync(root!);
                break;
            case CodeDialogStep.Update:
                Fill(_tracker.Shown);
                break;
            case CodeDialogStep.Close:
                _dialog?.Hide();
                break;
        }
    }

    async Task OpenAsync(XamlRoot root)
    {
        _title = new TextBlock { FontSize = 20, FontWeight = FontWeights.SemiBold, TextWrapping = TextWrapping.Wrap };
        _detail = new TextBlock { FontSize = 14, TextWrapping = TextWrapping.Wrap };
        AutomationProperties.SetLiveSetting(_detail, Microsoft.UI.Xaml.Automation.Peers.AutomationLiveSetting.Polite);
        var body = new StackPanel { Spacing = 12 };
        body.Children.Add(_title);
        body.Children.Add(_detail);
        var dialog = new ContentDialog
        {
            XamlRoot = root,
            Content = body,
            SecondaryButtonText = L.T("wireless.action.cancel"),
            DefaultButton = ContentDialogButton.None,
        };
        // "They Match" keeps the dialog open: it shows the code as confirmed until the iPhone answers.
        dialog.PrimaryButtonClick += (sender, args) =>
        {
            args.Cancel = true;
            Fill(_tracker.Matched());
            _ = _wireless.AnswerCodeAsync(CodeAnswer.Match);
        };
        _dialog = dialog;
        Fill(_tracker.Shown);
        bool shown;
        var cancelled = false;
        try
        {
            cancelled = await dialog.ShowAsync() == ContentDialogResult.Secondary;
            shown = true;
        }
        catch (Exception)
        {
            // Another dialog is up: this one could not be shown; the next change looks again.
            shown = false;
        }
        _dialog = null;
        var answer = _tracker.Ended(shown, cancelled);
        await _wireless.AnswerCodeAsync(answer);
        if (answer == CodeAnswer.Withdrawn && shown)
        {
            // Closed because the phase moved on: a new code may already be waiting.
            Follow();
        }
    }

    void Fill(CodePrompt? prompt)
    {
        if (_dialog is null || _title is null || _detail is null || prompt is null)
        {
            return;
        }
        _title.Text = prompt.Title;
        // Confirmed here: the DLL's waiting sentence at once, before the state says so too.
        _detail.Text = prompt.Confirmed ? L.T("wireless.pairing.confirmed") : prompt.Detail;
        // Confirmed here: only Cancel is left (an empty text hides the button).
        _dialog.PrimaryButtonText = prompt.Confirmed ? "" : L.T("wireless.action.match");
        AutomationProperties.SetName(_dialog, prompt.Title);
    }
}
