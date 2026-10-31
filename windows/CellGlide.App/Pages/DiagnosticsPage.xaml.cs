using System.ComponentModel;
using System.Diagnostics;
using CellGlide.App.Hosting;
using CellGlide.App.Shell;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.Windows.Storage.Pickers;

namespace CellGlide.App.Pages;

/// <summary>
/// Diagnostics (cg-gui pages/diagnostics.rs) over <see cref="DiagnosticsViewModel"/>. View asks the DLL for
/// the bundle of the form on screen and shows it, byte for byte, in the preview; Send and both saves work
/// only on that bundle, and any change to the form throws it away (<see cref="DiagnosticsViewModel.Discard"/>).
/// The form is frozen while a bundle is built, sent or saved, so a reply always belongs to what is on screen.
/// Texts, the file name and every failure sentence are the DLL's.
/// </summary>
public sealed partial class DiagnosticsPage : Page
{
    readonly DiagnosticsViewModel _diag;

    public DiagnosticsPage(DiagnosticsViewModel diag)
    {
        _diag = diag;
        InitializeComponent();
        diag.PropertyChanged += OnChanged;
        foreach (var command in new INotifyPropertyChanged[] { diag.PrepareCommand, diag.SendCommand, diag.SaveCommand })
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
        Render();
    }

    bool Busy => _diag.PrepareCommand.IsRunning || _diag.SendCommand.IsRunning || _diag.SaveCommand.IsRunning;

    void OnChanged(object? sender, PropertyChangedEventArgs e) => Render();

    void Render()
    {
        var busy = Busy;
        var json = _diag.Json;

        DayRange.IsEnabled = WeekRange.IsEnabled = Email.IsEnabled = Note.IsEnabled = !busy;
        ViewButton.IsEnabled = !busy;
        SendButton.IsEnabled = !busy && json is not null;
        SaveButton.IsEnabled = SaveAsButton.IsEnabled = !busy && json is not null;
        NeedsPreview.Visibility = Show.Visible(!busy && json is null);

        ErrorLine.Text = _diag.Error ?? "";
        ErrorLine.Foreground = Show.Brush(Tone.Red);
        ErrorLine.Visibility = Show.Visible(_diag.Error);
        var busyLine = _diag.PrepareCommand.IsRunning ? L.T("diag.preparing")
            : _diag.SendCommand.IsRunning ? L.T("diag.sending")
            : null;
        BusyLine.Text = busyLine ?? "";
        BusyLine.Visibility = Show.Visible(busyLine);

        // The same text, so it is not set again (which would move the reader's caret) on every change.
        if (Preview.Text != (json ?? ""))
        {
            Preview.Text = json ?? "";
        }
        PreviewCard.Visibility = Show.Visible(json);

        SavedLine.Text = _diag.SavedPath is { } path ? L.T("diag.saved", "path", path) : "";
        SavedRow.Visibility = Show.Visible(_diag.SavedPath);

        CodeText.Text = _diag.Code ?? "";
        SentDot.Fill = Show.Brush(Tone.Green);
        SentCard.Visibility = Show.Visible(_diag.Code);
    }

    DiagnosticsInput Input() =>
        new(WeekRange.IsChecked == true ? DiagRange.Week : DiagRange.Day, Email.Text, Note.Text);

    /// <summary>The form changed: the bundle on hand is no longer the one for it.</summary>
    void FormChanged()
    {
        if (_diag is not null && _diag.Json is not null && !Busy)
        {
            _diag.Discard();
        }
    }

    void OnRangeChecked(object sender, RoutedEventArgs e) => FormChanged();

    void OnFieldChanged(object sender, TextChangedEventArgs e) => FormChanged();

    async void OnViewClick(object sender, RoutedEventArgs e)
    {
        if (Busy)
        {
            return;
        }
        if (_diag.Json is null)
        {
            // A failure leaves the DLL's sentence in Error.
            await _diag.PrepareCommand.ExecuteAsync(Input());
        }
        if (_diag.Json is not null)
        {
            Preview.Focus(FocusState.Programmatic);
            Preview.StartBringIntoView();
        }
    }

    async void OnSendClick(object sender, RoutedEventArgs e)
    {
        if (!Busy && _diag.SendCommand.CanExecute(null))
        {
            await _diag.SendCommand.ExecuteAsync(null);
        }
    }

    /// <summary>To the Downloads folder, under the DLL's name; a file already there is never replaced (-2 and so on).</summary>
    async void OnSaveClick(object sender, RoutedEventArgs e)
    {
        if (!Busy && _diag.SaveCommand.CanExecute(null))
        {
            await _diag.SaveCommand.ExecuteAsync(null);
        }
    }

    /// <summary>
    /// Anywhere the user picks, offered under the DLL's file name as .json. The dialog asks before an existing
    /// file is chosen, so a chosen file that exists is one the user agreed to replace.
    /// </summary>
    async void OnSaveAsClick(object sender, RoutedEventArgs e)
    {
        if (Busy || !_diag.SaveCommand.CanExecute(null) || XamlRoot?.ContentIslandEnvironment is not { } island)
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
}
