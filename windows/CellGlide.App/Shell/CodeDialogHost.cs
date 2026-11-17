using CellGlide.App.Hosting;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace CellGlide.App.Shell;

/// <summary>
/// The code dialog, in the main window only (one at a time; the panel shows "Check the Code", which opens the
/// window and this): "Confirm the code 384 271 on the iPhone" and "Compare it with the number on the iPhone's
/// screen", with "They Match" and "Cancel". When it opens, updates and closes, and what its answers send, is
/// <see cref="CodeDialogController"/>'s (tested without WinUI); this is only the ContentDialog. "They Match"
/// keeps it open, showing "Waiting for the iPhone to confirm too…" and only Cancel; a refused confirmation
/// shows the question again with the reason. No default button: a code is compared, never confirmed by a
/// reflexive Enter. A dialog that could not be shown (another was up) is tried again when another closes.
/// </summary>
public sealed class CodeDialogHost : ICodeDialogPresenter
{
    readonly Func<XamlRoot?> _root;
    readonly Func<bool> _onScreen;
    readonly CodeDialogController _controller;
    ContentDialog? _dialog;
    TextBlock? _title;
    TextBlock? _detail;
    TextBlock? _error;

    /// <param name="root">The main window's XAML root, once its content is loaded.</param>
    /// <param name="onScreen">Whether the main window is shown.</param>
    public CodeDialogHost(WirelessViewModel wireless, Func<XamlRoot?> root, Func<bool> onScreen)
    {
        _root = root;
        _onScreen = onScreen;
        _controller = new CodeDialogController(wireless, this);
        // The app's other dialogs go through Dialogs: when one closes, a code dialog that could not be shown can be.
        Dialogs.Closed += _controller.Retry;
    }

    public event Action? Matched;

    public bool CanShow => _onScreen() && _root() is not null;

    /// <summary>"Check the Code": opens the dialog even after Esc put it away.</summary>
    public void Ask() => _controller.Ask();

    /// <summary>Looks again: the window was shown, or its content loaded.</summary>
    public void Follow() => _controller.Retry();

    public async Task<CodeDialogClose> ShowAsync(CodePrompt prompt)
    {
        if (_root() is not { } root)
        {
            return CodeDialogClose.NotShown;
        }
        _title = new TextBlock { FontSize = 20, FontWeight = FontWeights.SemiBold, TextWrapping = TextWrapping.Wrap };
        _detail = new TextBlock { FontSize = 14, TextWrapping = TextWrapping.Wrap };
        _error = new TextBlock { FontSize = 13, TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed };
        AutomationProperties.SetLiveSetting(_detail, Microsoft.UI.Xaml.Automation.Peers.AutomationLiveSetting.Polite);
        AutomationProperties.SetLiveSetting(_error, Microsoft.UI.Xaml.Automation.Peers.AutomationLiveSetting.Assertive);
        var body = new StackPanel { Spacing = 12 };
        body.Children.Add(_title);
        body.Children.Add(_detail);
        body.Children.Add(_error);
        var dialog = new ContentDialog
        {
            XamlRoot = root,
            Content = body,
            SecondaryButtonText = L.T("wireless.action.cancel"),
            DefaultButton = ContentDialogButton.None,
        };
        // "They Match" keeps the dialog open: the controller shows the code as confirmed until the iPhone answers.
        dialog.PrimaryButtonClick += (sender, args) =>
        {
            args.Cancel = true;
            Matched?.Invoke();
        };
        _dialog = dialog;
        Fill(prompt, null);
        var result = await Dialogs.ShowAsync(dialog);
        _dialog = null;
        return result switch
        {
            null => CodeDialogClose.NotShown,
            ContentDialogResult.Secondary => CodeDialogClose.Cancelled,
            _ => CodeDialogClose.Dismissed,
        };
    }

    public void Fill(CodePrompt prompt, string? error)
    {
        if (_dialog is null || _title is null || _detail is null || _error is null)
        {
            return;
        }
        _title.Text = prompt.Title;
        // Confirmed here: the DLL's waiting sentence at once, before the state says so too.
        _detail.Text = prompt.Confirmed ? L.T("wireless.pairing.confirmed") : prompt.Detail;
        _error.Text = error ?? "";
        _error.Foreground = Show.Brush(Tone.Red);
        _error.Visibility = Show.Visible(error);
        // Confirmed here: only Cancel is left (an empty text hides the button).
        _dialog.PrimaryButtonText = prompt.Confirmed ? "" : L.T("wireless.action.match");
        AutomationProperties.SetName(_dialog, prompt.Title);
    }

    public void Hide() => _dialog?.Hide();
}
