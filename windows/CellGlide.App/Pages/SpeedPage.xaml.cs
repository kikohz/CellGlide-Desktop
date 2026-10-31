using System.ComponentModel;
using CellGlide.App.Hosting;
using CellGlide.App.Shell;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Windows.Foundation;

namespace CellGlide.App.Pages;

/// <summary>
/// The speed test (cg-gui pages/speed.rs) drawn from <see cref="SpeedViewModel.View"/>: the parts that
/// follow the view are filled in code each time it changes (x:Bind would stop calling a function whose
/// argument path is null, and the legs and rows come and go). Which text and tone to show is always
/// the view's; the code only lays it out.
/// <para>
/// The data warning: the DLL puts it up (<see cref="SpeedViewModel.DataWarningRequested"/>), the page
/// asks with a dialog. The answer goes to <see cref="SpeedViewModel.AnswerDataWarningAsync"/>, which decides what to
/// send (Continue: SetPrefs first when "don't ask again" is ticked, then SpeedStart confirmed; Cancel or Esc:
/// SpeedCancel, which puts it away in the DLL).
/// When the DLL puts it away by itself (the phone left), the dialog closes without sending anything.
/// </para>
/// </summary>
public sealed partial class SpeedPage : Page
{
    /// <summary>cg-gui's curve height.</summary>
    const double CurveHeight = 32;

    readonly SpeedViewModel _speed;
    readonly SettingsViewModel _settings;
    readonly List<LegColumn> _legs = [];
    /// <summary>The data warning dialog while it is up.</summary>
    ContentDialog? _warning;
    /// <summary>The DLL put the warning away: closing the dialog sends nothing.</summary>
    bool _warningWithdrawn;
    /// <summary>The DLL asked while the page was not shown: ask once it is.</summary>
    bool _warningPending;
    /// <summary>A change of preferences made here that failed (the DLL's sentence), until the next action.</summary>
    string? _prefsError;

    public SpeedPage(SpeedViewModel speed, SettingsViewModel settings)
    {
        _speed = speed;
        _settings = settings;
        InitializeComponent();
        speed.PropertyChanged += OnChanged;
        settings.PropertyChanged += OnChanged;
        speed.DataWarningRequested += OnDataWarning;
        Loaded += (_, _) =>
        {
            // Loaded can come twice in a row without Unloaded between: take any earlier subscription off first,
            // so there is never more than one.
            Show.Changed -= Repaint;
            Show.Changed += Repaint;
            Render();
            if (_warningPending)
            {
                _ = AskAsync();
            }
        };
        Unloaded += (_, _) => Show.Changed -= Repaint;
        Show.AnnounceChanges(ErrorLine);
        Show.AnnounceChanges(Verdict);
        Render();
    }

    void OnChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(SpeedViewModel.View) or nameof(CoreViewModel.Error) or nameof(SettingsViewModel.Prefs))
        {
            Render();
        }
    }

    /// <summary>The tone brushes changed (high contrast): draw the columns and rows again with the new ones.</summary>
    void Repaint()
    {
        Legs.Tag = null;
        History.Tag = null;
        Render();
    }

    void Render()
    {
        var view = _speed.View;
        var running = view?.Running == true;
        var prefs = _settings.Prefs;

        // "Also test Wi-Fi or Ethernet (…)": offered by the DLL, ticked as the preference says, fixed while running.
        if (view is { OffersWifi: true, Uplink: { } uplink })
        {
            IncludeWifi.Content = L.T("speed.include_wifi", "iface", uplink);
            IncludeWifi.IsChecked = prefs?.IncludeWifi == true;
            IncludeWifi.IsEnabled = !running && prefs is not null;
            IncludeWifi.Visibility = Visibility.Visible;
        }
        else
        {
            IncludeWifi.Visibility = Visibility.Collapsed;
        }

        StartButton.Content = L.T(running ? "common.cancel" : "speed.start");
        StartButton.Style = (Style)Application.Current.Resources[running ? "DefaultButtonStyle" : "AccentButtonStyle"];
        StartButton.IsEnabled = view?.StartEnabled == true;
        StartNote.Text = view?.StartNote ?? "";
        StartNote.Visibility = Show.Visible(view?.StartNote);

        var error = _speed.Error ?? _prefsError;
        ErrorLine.Text = error ?? "";
        ErrorLine.Foreground = Show.Brush(Tone.Red);
        ErrorLine.Visibility = Show.Visible(error);

        RenderLegs(view, running);
        Verdict.Text = view?.Verdict ?? "";
        VerdictBlock.Visibility = Show.Visible(view?.Verdict);
        RenderHistory(view, running);

        // The DLL put the warning away by itself (the phone left): so does the dialog.
        if (_warning is not null && view?.Dialog != SpeedDialog.DataWarning)
        {
            _warningWithdrawn = true;
            _warning.Hide();
        }
    }

    // ---- The routes ----

    void RenderLegs(SpeedView? view, bool running)
    {
        var legs = view?.Legs ?? [];
        ResultsCard.Visibility = Show.Visible(legs.Count > 0);
        // Without a phone leg, cg-gui still shows the iPhone's column, saying none is connected.
        var noPhone = legs.Count > 0 && !legs.Any(leg => leg.Phone);
        var shape = (noPhone ? "n" : "") + string.Concat(legs.Select(leg => leg.Phone ? "p" : "w"));
        if ((string?)Legs.Tag != shape)
        {
            // Other routes than shown: build the columns again (otherwise they are updated in place,
            // so a running bar keeps its animation).
            Legs.Tag = shape;
            Legs.Children.Clear();
            Legs.ColumnDefinitions.Clear();
            _legs.Clear();
            var column = 0;
            if (noPhone)
            {
                AddColumn(NoPhoneColumn(), ref column);
            }
            foreach (var _ in legs)
            {
                var leg = new LegColumn();
                _legs.Add(leg);
                AddColumn(leg.Root, ref column);
            }
        }
        for (var i = 0; i < legs.Count; i++)
        {
            _legs[i].Update(legs[i], running);
        }
    }

    /// <summary>Adds a column, after a hairline when it is not the first.</summary>
    void AddColumn(FrameworkElement content, ref int column)
    {
        if (column > 0)
        {
            Legs.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            var rule = new Rectangle { Width = 1, Fill = (Brush)Application.Current.Resources["DividerStrokeColorDefaultBrush"] };
            Grid.SetColumn(rule, column++);
            Legs.Children.Add(rule);
        }
        Legs.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        Grid.SetColumn(content, column++);
        Legs.Children.Add(content);
    }

    static FrameworkElement NoPhoneColumn()
    {
        var column = new StackPanel { Spacing = 8 };
        column.Children.Add(RouteName(L.T("speed.phone"), phone: true, 13));
        column.Children.Add(Secondary(L.T("speed.no_phone"), 13));
        return column;
    }

    /// <summary>A coloured dot and the route's name: iPhone blue, Wi-Fi or Ethernet green.</summary>
    static StackPanel RouteName(string name, bool phone, double size)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        var dot = new Ellipse { Width = 7, Height = 7, VerticalAlignment = VerticalAlignment.Center, Fill = Show.Brush(phone ? Tone.Blue : Tone.Green) };
        AutomationProperties.SetAccessibilityView(dot, AccessibilityView.Raw);
        row.Children.Add(dot);
        row.Children.Add(new TextBlock { Text = name, FontSize = size, TextWrapping = TextWrapping.Wrap });
        return row;
    }

    static TextBlock Secondary(string text, double size) => new()
    {
        Text = text,
        FontSize = size,
        TextWrapping = TextWrapping.Wrap,
        Foreground = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"],
    };

    /// <summary>
    /// One route's column (cg-gui leg_column): its name, the numbers so far, the download curve, a bar
    /// while it measures (busy while upload waits for the server), the stage, why it failed, the notes.
    /// </summary>
    sealed class LegColumn
    {
        readonly StackPanel _name;
        readonly TextBlock _numbers;
        readonly Canvas _curve;
        readonly ProgressBar _bar;
        readonly TextBlock _stage;
        readonly TextBlock _error;
        readonly StackPanel _notes;
        SpeedLegView? _leg;

        public LegColumn()
        {
            Root = new StackPanel { Spacing = 8 };
            _name = new StackPanel();
            _numbers = new TextBlock { FontSize = 13, TextWrapping = TextWrapping.Wrap };
            _curve = new Canvas { Height = CurveHeight, HorizontalAlignment = HorizontalAlignment.Stretch };
            // The curve is a picture of the numbers above it.
            AutomationProperties.SetAccessibilityView(_curve, AccessibilityView.Raw);
            _curve.SizeChanged += (_, _) => DrawCurve();
            _bar = new ProgressBar { Maximum = 100, Height = 4 };
            _stage = Secondary("", 12);
            _error = new TextBlock { FontSize = 12, TextWrapping = TextWrapping.Wrap };
            _notes = new StackPanel { Spacing = 4 };
            foreach (var part in new UIElement[] { _name, _numbers, _curve, _bar, _stage, _error, _notes })
            {
                Root.Children.Add(part);
            }
        }

        public StackPanel Root { get; }

        public void Update(SpeedLegView leg, bool running)
        {
            var previous = _leg;
            _leg = leg;
            if (previous?.Label != leg.Label || previous.Phone != leg.Phone)
            {
                _name.Children.Clear();
                _name.Children.Add(RouteName(leg.Label, leg.Phone, 13));
            }
            var failed = leg.Stage == StageView.Failed;
            _numbers.Text = leg.Numbers;
            _numbers.Visibility = Show.Visible(!failed && leg.Numbers.Length > 0);
            _curve.Visibility = Show.Visible(!failed);
            DrawCurve();

            var measuring = leg.Stage is StageView.Latency or StageView.Download or StageView.Upload;
            _bar.Visibility = Show.Visible(running && measuring && !failed);
            _bar.IsIndeterminate = leg.Waiting;
            _bar.Value = leg.Progress * 100;
            AutomationProperties.SetName(_bar, leg.StageText ?? leg.Label);

            _stage.Text = leg.StageText ?? "";
            _stage.Visibility = Show.Visible(!failed && leg.StageText is { Length: > 0 });
            _error.Text = leg.Error ?? "";
            _error.Foreground = Show.Brush(Tone.Red);
            _error.Visibility = Show.Visible(leg.Error);

            // Every frame repeats the notes: rebuild them only when they changed.
            IReadOnlyList<string> notes = failed ? [] : leg.Notes;
            if (!_notes.Children.OfType<TextBlock>().Select(note => note.Text).SequenceEqual(notes))
            {
                _notes.Children.Clear();
                foreach (var note in notes)
                {
                    _notes.Children.Add(new TextBlock { Text = note, FontSize = 11, TextWrapping = TextWrapping.Wrap, Foreground = Show.Brush(Tone.Orange) });
                }
            }
        }

        void DrawCurve()
        {
            _curve.Children.Clear();
            var width = _curve.ActualWidth;
            if (_leg is not { } leg || leg.Curve.Count == 0 || width <= 0)
            {
                return;
            }
            var line = new Polyline
            {
                Stroke = Show.Brush(leg.Phone ? Tone.Blue : Tone.Green),
                StrokeThickness = 1.5,
                StrokeLineJoin = PenLineJoin.Round,
            };
            foreach (var p in Core.Desktop.Geometry.Plot(leg.Curve, width, CurveHeight))
            {
                line.Points.Add(new Point(p.X, p.Y));
            }
            _curve.Children.Add(line);
        }
    }

    // ---- The recent results ----

    void RenderHistory(SpeedView? view, bool running)
    {
        var records = view?.History ?? [];
        HistoryFailed.Visibility = Show.Visible(view?.HistoryFailed == true);
        HistoryEmpty.Visibility = Show.Visible(records.Count == 0);
        ClearButton.Visibility = Show.Visible(records.Count > 0);
        ClearButton.IsEnabled = !running;
        History.Visibility = Show.Visible(records.Count > 0);
        // Every frame brings the same list again: rebuild only when it changed.
        if (History.Tag is IReadOnlyList<SpeedRecordView> shown && shown.SequenceEqual(records))
        {
            return;
        }
        History.Tag = records;
        History.Children.Clear();
        History.RowDefinitions.Clear();
        if (records.Count == 0)
        {
            return;
        }
        var row = 0;
        Add(new Border(), row, 0);
        Add(RouteName(L.T("speed.phone"), phone: true, 11), row, 1);
        Add(RouteName(L.T("speed.wifi_short"), phone: false, 11), row, 2);
        foreach (var record in records)
        {
            row++;
            var rule = new Rectangle { Height = 1, Fill = (Brush)Application.Current.Resources["DividerStrokeColorDefaultBrush"] };
            Add(rule, row, 0);
            Grid.SetColumnSpan(rule, 3);
            row++;
            var when = new StackPanel { Spacing = 2 };
            when.Children.Add(new TextBlock { Text = record.Date, FontSize = 12, TextWrapping = TextWrapping.Wrap });
            if (record.Carrier is { } carrier)
            {
                when.Children.Add(Secondary(carrier, 11));
            }
            Add(when, row, 0);
            Add(new TextBlock { Text = record.Phone ?? "", FontSize = 12, TextWrapping = TextWrapping.Wrap }, row, 1);
            Add(new TextBlock { Text = record.Wifi ?? "", FontSize = 12, TextWrapping = TextWrapping.Wrap }, row, 2);
        }
    }

    void Add(FrameworkElement element, int row, int column)
    {
        while (History.RowDefinitions.Count <= row)
        {
            History.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        }
        Grid.SetRow(element, row);
        Grid.SetColumn(element, column);
        History.Children.Add(element);
    }

    // ---- Actions ----

    async void OnIncludeWifiClick(object sender, RoutedEventArgs e)
    {
        var on = IncludeWifi.IsChecked == true;
        // The box then shows the preference as it is: the new one when it comes back from the DLL,
        // the old one when the change failed.
        await SetPrefsAsync(prefs => prefs with { IncludeWifi = on });
    }

    async void OnStartClick(object sender, RoutedEventArgs e)
    {
        _prefsError = null;
        if (_speed.View?.Running == true)
        {
            await _speed.CancelCommand.ExecuteAsync(null);
        }
        else
        {
            // Unconfirmed: when the test would use phone data, the DLL puts the warning up instead.
            await _speed.StartCommand.ExecuteAsync(false);
        }
    }

    async void OnClearClick(object sender, RoutedEventArgs e)
    {
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Content = new TextBlock { Text = L.T("speed.clear_confirm"), FontSize = 14, FontWeight = FontWeights.SemiBold, TextWrapping = TextWrapping.Wrap },
            PrimaryButtonText = L.T("common.clear"),
            CloseButtonText = L.T("common.cancel"),
            DefaultButton = ContentDialogButton.Close,
        };
        if (await ShowAsync(dialog) == ContentDialogResult.Primary)
        {
            await _speed.ClearHistoryCommand.ExecuteAsync(null);
        }
    }

    void OnDataWarning()
    {
        _warningPending = true;
        if (IsLoaded)
        {
            _ = AskAsync();
        }
    }

    async Task AskAsync()
    {
        _warningPending = false;
        if (_warning is not null || _speed.View?.Dialog != SpeedDialog.DataWarning)
        {
            return;
        }
        var head = new Grid { ColumnSpacing = 8 };
        head.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        head.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        var dot = new Ellipse { Width = 10, Height = 10, VerticalAlignment = VerticalAlignment.Center, Fill = Show.Brush(Tone.Orange) };
        AutomationProperties.SetAccessibilityView(dot, AccessibilityView.Raw);
        var text = new TextBlock { Text = L.T("speed.warning"), FontSize = 14, FontWeight = FontWeights.SemiBold, TextWrapping = TextWrapping.Wrap };
        Grid.SetColumn(text, 1);
        head.Children.Add(dot);
        head.Children.Add(text);
        var dontAsk = new CheckBox { Content = L.T("speed.dont_ask") };
        var body = new StackPanel { Spacing = 14 };
        body.Children.Add(head);
        body.Children.Add(dontAsk);
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Content = body,
            PrimaryButtonText = L.T("common.continue"),
            CloseButtonText = L.T("common.cancel"),
            DefaultButton = ContentDialogButton.Primary,
        };
        AutomationProperties.SetName(dialog, L.T("speed.warning"));
        _warning = dialog;
        _warningWithdrawn = false;
        var answer = await ShowAsync(dialog);
        _warning = null;
        // What the answer sends is the view model's (Cancel, Esc or a dialog that could not be shown is a
        // dismissal; a warning the DLL took back sends nothing).
        var choice = _warningWithdrawn ? DataWarningAnswer.Withdrawn
            : answer == ContentDialogResult.Primary ? DataWarningAnswer.Continue
            : DataWarningAnswer.Dismiss;
        _warningWithdrawn = false;
        _prefsError = null;
        var dontAskAgain = dontAsk.IsChecked == true;
        await _speed.AnswerDataWarningAsync(choice, dontAskAgain);
        if (choice == DataWarningAnswer.Continue && dontAskAgain && _settings.Error is { } failed)
        {
            // The preference could not be saved (the test started anyway): say so.
            _prefsError = failed;
            Render();
        }
    }

    /// <summary>Shows a dialog; one that cannot be shown (another is up) counts as dismissed.</summary>
    static async Task<ContentDialogResult> ShowAsync(ContentDialog dialog)
    {
        try
        {
            return await dialog.ShowAsync();
        }
        catch (Exception)
        {
            return ContentDialogResult.None;
        }
    }

    /// <summary>Changes the app's preferences (read first if they never came); a failure is shown on the page.</summary>
    async Task SetPrefsAsync(Func<Prefs, Prefs> change)
    {
        _prefsError = null;
        var prefs = _settings.Prefs ?? await _settings.LoadAsync();
        if (prefs is not null)
        {
            await _settings.SetPrefsCommand.ExecuteAsync(change(prefs));
        }
        _prefsError = _settings.Error;
        if (_prefsError is not null)
        {
            Render();
        }
    }
}
