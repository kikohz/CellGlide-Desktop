using System.ComponentModel;
using System.Diagnostics;
using CellGlide.App.Hosting;
using CellGlide.App.Shell;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.Windows.Storage.Pickers;
using Windows.ApplicationModel.DataTransfer;

namespace CellGlide.App.Pages;

/// <summary>
/// Diagnostics (cg-gui pages/diagnostics.rs) over <see cref="DiagnosticsViewModel"/>. View asks the DLL for
/// the bundle of the form on screen and shows it, byte for byte, in the preview; only a previewed bundle can
/// be sent. Saving needs no preview: it builds the bundle first (which then still needs a view before a send).
/// Any change to the form throws the bundle away (<see cref="DiagnosticsViewModel.Discard"/>). The form is
/// frozen while a bundle is built, sent or saved, so a reply always belongs to what is on screen. After a send
/// the sent card replaces the form: copy the code, Send Another (keeps the email and note) or Done (clears
/// them). Texts, the file name and every failure sentence are the DLL's.
/// </summary>
public sealed partial class DiagnosticsPage : Page
{
    /// <summary>How long "Copied" shows before turning back into "Copy" (cg-gui COPIED_FOR).</summary>
    static readonly TimeSpan CopiedFor = TimeSpan.FromSeconds(2);

    readonly DiagnosticsViewModel _diag;
    // "Copied" is the page's own short-lived state; the number lets only the last copy's timer turn it back.
    int _copies;
    bool _copied;

    public DiagnosticsPage(DiagnosticsViewModel diag)
    {
        _diag = diag;
        InitializeComponent();
        diag.PropertyChanged += OnChanged;
        foreach (var command in new INotifyPropertyChanged[] { diag.PrepareCommand, diag.PrepareToSaveCommand, diag.SendCommand, diag.SaveCommand })
        {
            command.PropertyChanged += (_, e) =>
            {
                if (e.PropertyName == "IsRunning")
                {
                    Render();
                }
            };
        }
        Loaded += (_, _) =>
        {
            Show.Changed -= Render;
            Show.Changed += Render;
            Render();
        };
        Unloaded += (_, _) => Show.Changed -= Render;
        Show.AnnounceChanges(ErrorLine);
        Show.AnnounceChanges(BusyLine);
        Show.AnnounceChanges(SavedLine);
        Show.AnnounceChanges(CodeText);
        Render();
    }

    bool Busy =>
        _diag.PrepareCommand.IsRunning || _diag.PrepareToSaveCommand.IsRunning || _diag.SendCommand.IsRunning || _diag.SaveCommand.IsRunning;

    void OnChanged(object? sender, PropertyChangedEventArgs e) => Render();

    void Render()
    {
        var busy = Busy;
        var sent = _diag.Code is not null;
        var canSend = !busy && _diag.SendCommand.CanExecute(null);

        // The sent card in place of the form.
        SentCard.Visibility = Show.Visible(sent);
        FormCard.Visibility = Show.Visible(!sent);
        CodeText.Text = _diag.Code ?? "";
        SentDot.Fill = Show.Brush(Tone.Green);
        CopyCodeButton.Content = L.T(_copied ? "common.copied" : "common.copy");
        AutomationProperties.SetName(CopyCodeButton, L.T(_copied ? "common.copied" : "common.copy"));

        // All at once, so a control going off hands focus to a neighbour that is on in the new state; the one that
        // started the work gets focus back when it is done.
        Show.SetEnabled(
            [
                (CopyCodeButton, true), (AgainButton, !busy), (DoneButton, !busy),
                (Range, !busy), (Email, !busy), (Note, !busy),
                (SaveButton, !busy), (SaveAsButton, !busy), (ViewButton, !busy), (SendButton, canSend),
            ],
            [CopyCodeButton, AgainButton, DoneButton, Range, Email, Note, SaveButton, SaveAsButton, ViewButton, SendButton, OpenFolderButton, Preview]);
        NeedsPreview.Visibility = Show.Visible(!busy && !canSend);

        ErrorLine.Text = _diag.Error ?? "";
        ErrorLine.Foreground = Show.Brush(Tone.Red);
        ErrorLine.Visibility = Show.Visible(_diag.Error);
        var busyLine = _diag.PrepareCommand.IsRunning || _diag.PrepareToSaveCommand.IsRunning ? L.T("diag.preparing")
            : _diag.SendCommand.IsRunning ? L.T("diag.sending")
            : null;
        BusyLine.Text = busyLine ?? "";
        BusyLine.Visibility = Show.Visible(busyLine);

        // Only a previewed bundle is shown (and so may be sent); one built for a save stays out of sight.
        var shown = _diag.Previewed ? _diag.Json ?? "" : "";
        // The same text is not set again (which would move the reader's caret) on every change.
        if (Preview.Text != shown)
        {
            Preview.Text = shown;
        }
        PreviewCard.Visibility = Show.Visible(_diag.Previewed && _diag.Json is not null && !sent);

        SavedLine.Text = _diag.SavedPath is { } path ? L.T("diag.saved", "path", path) : "";
        SavedRow.Visibility = Show.Visible(_diag.SavedPath);
    }

    DiagnosticsInput Input() =>
        new(Range.SelectedIndex == 1 ? DiagRange.Week : DiagRange.Day, Email.Text, Note.Text);

    /// <summary>The form changed: the bundle on hand is no longer the one for it.</summary>
    void FormChanged()
    {
        if (_diag is not null && _diag.Json is not null && !Busy)
        {
            _diag.Discard();
        }
    }

    void OnRangeChanged(object sender, SelectionChangedEventArgs e) => FormChanged();

    void OnFieldChanged(object sender, TextChangedEventArgs e) => FormChanged();

    async void OnViewClick(object sender, RoutedEventArgs e)
    {
        if (Busy)
        {
            return;
        }
        // Builds the bundle unless one is on hand (one built for a save is shown as it is); a failure leaves
        // the DLL's sentence in Error.
        await _diag.PrepareCommand.ExecuteAsync(Input());
        if (_diag.Previewed)
        {
            Preview.Focus(FocusState.Programmatic);
            Preview.StartBringIntoView();
        }
    }

    async void OnSendClick(object sender, RoutedEventArgs e)
    {
        if (Busy || !_diag.SendCommand.CanExecute(null))
        {
            return;
        }
        // The form is off while sending, so focus has nowhere to stay: put it back where the result is.
        await _diag.SendCommand.ExecuteAsync(null);
        if (_diag.Code is not null)
        {
            CopyCodeButton.Focus(FocusState.Programmatic);
        }
        else if (SendButton.IsEnabled)
        {
            SendButton.Focus(FocusState.Programmatic);
        }
    }

    /// <summary>
    /// To the Downloads folder, under the DLL's name; a file already there is never replaced (-2 and so on).
    /// Without a bundle on hand, the bundle of the form is built first.
    /// </summary>
    async void OnSaveClick(object sender, RoutedEventArgs e)
    {
        if (Busy)
        {
            return;
        }
        await _diag.PrepareToSaveCommand.ExecuteAsync(Input());
        if (!Busy && _diag.SaveCommand.CanExecute(null))
        {
            await _diag.SaveCommand.ExecuteAsync(null);
        }
    }

    /// <summary>
    /// Anywhere the user picks, offered under the DLL's file name as .json (the bundle is built first when there
    /// is none, so the name is known). The dialog asks before an existing file is chosen, so a chosen file that
    /// exists is one the user agreed to replace.
    /// </summary>
    async void OnSaveAsClick(object sender, RoutedEventArgs e)
    {
        if (Busy || XamlRoot?.ContentIslandEnvironment is not { } island)
        {
            return;
        }
        await _diag.PrepareToSaveCommand.ExecuteAsync(Input());
        if (Busy || !_diag.SaveCommand.CanExecute(null))
        {
            return;
        }
        string? path;
        try
        {
            var picker = new FileSavePicker(island.AppWindowId)
            {
                SuggestedStartLocation = PickerLocationId.Downloads,
                SuggestedFileName = Path.GetFileNameWithoutExtension(_diag.FileName ?? ""),
                DefaultFileExtension = ".json",
            };
            // A List<string> written out: CsWinRT has no COM wrapper for what a collection expression makes here
            // (InvalidCastException at run time, which crashed the app before this was caught).
            picker.FileTypeChoices.Add(L.T("diag.file_type"), new List<string> { ".json" });
            path = (await picker.PickSaveFileAsync())?.Path;
        }
        catch (Exception ex)
        {
            Debug.WriteLine("CellGlide: the save dialog failed: " + ex);
            return;
        }
        // The bundle may have been thrown away while the dialog was open; Busy covers a click that came meanwhile.
        if (string.IsNullOrEmpty(path) || Busy || !_diag.SaveCommand.CanExecute(null))
        {
            return;
        }
        await _diag.SaveCommand.ExecuteAsync(new DiagnosticsSaveTarget(path, File.Exists(path)));
    }

    /// <summary>Explorer with the saved file selected; the path is an argument of its own, never through a shell.</summary>
    void OnOpenFolderClick(object sender, RoutedEventArgs e)
    {
        if (_diag.SavedPath is not { } path)
        {
            return;
        }
        var explorer = new ProcessStartInfo(Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Windows), "explorer.exe"))
        {
            UseShellExecute = false,
        };
        explorer.ArgumentList.Add("/select,");
        explorer.ArgumentList.Add(path);
        try
        {
            Process.Start(explorer)?.Dispose();
        }
        catch (Exception ex)
        {
            Debug.WriteLine("CellGlide: opening the folder failed: " + ex);
        }
    }

    /// <summary>Copies the report code; the button says "Copied" for 2 s (cg-gui copying_the_code_turns_back_after_two_seconds).</summary>
    async void OnCopyCodeClick(object sender, RoutedEventArgs e)
    {
        if (_diag.Code is not { } code)
        {
            return;
        }
        var package = new DataPackage();
        package.SetText(code);
        try
        {
            Clipboard.SetContent(package);
            Clipboard.Flush();
        }
        catch (Exception)
        {
            // The clipboard is held by another program right now: nothing copied, nothing claimed.
            return;
        }
        _copied = true;
        var copy = ++_copies;
        Render();
        if ((FrameworkElementAutomationPeer.FromElement(CopyCodeButton) ?? FrameworkElementAutomationPeer.CreatePeerForElement(CopyCodeButton)) is { } peer)
        {
            peer.RaiseAutomationEvent(AutomationEvents.LiveRegionChanged);
        }
        await Task.Delay(CopiedFor);
        if (copy == _copies)
        {
            _copied = false;
            Render();
        }
    }

    /// <summary>"Send Another": a new report with the same email and note.</summary>
    void OnAgainClick(object sender, RoutedEventArgs e)
    {
        if (!Busy)
        {
            Restart();
        }
    }

    /// <summary>"Done": the report is finished; the form is cleared.</summary>
    void OnDoneClick(object sender, RoutedEventArgs e)
    {
        if (Busy)
        {
            return;
        }
        Email.Text = "";
        Note.Text = "";
        Restart();
    }

    void Restart()
    {
        _diag.Reset();
        _copied = false;
        _copies++;
        Render();
        ViewButton.Focus(FocusState.Programmatic);
    }
}
