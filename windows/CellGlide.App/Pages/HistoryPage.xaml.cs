using System.ComponentModel;
using CellGlide.App.Shell;
using CellGlide.Core.Desktop;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;

namespace CellGlide.App.Pages;

/// <summary>
/// Connection history (cg-gui pages/history.rs) drawn from <see cref="HistoryViewModel.View"/>, filled in
/// code each time the view changes (x:Bind would stop calling a function whose argument path is null). The
/// day's label, its neighbours, strip cells, hour labels, legend and rows are the DLL's. Showing the page
/// asks for today; while it shows in a visible, unminimised window (<see cref="HistoryRefreshGate"/>), today is
/// asked for again every 5 s (the view model does nothing when another day is on screen or a load is out), and
/// when the window comes back from the tray or from minimised, today is asked for at once.
/// </summary>
public sealed partial class HistoryPage : Page
{
    /// <summary>cg-gui's refresh while the page is open.</summary>
    static readonly TimeSpan RefreshEvery = TimeSpan.FromSeconds(5);
    /// <summary>Wide enough for "23:59" in the row's text (cg-gui TIME_WIDTH).</summary>
    const double TimeWidth = 44;

    readonly HistoryViewModel _history;
    readonly DispatcherTimer _timer = new() { Interval = RefreshEvery };
    readonly HistoryRefreshGate _gate = new();
    readonly MainWindow _window;
    bool _pageShown;
    // The rows on screen and the day they belong to, so a refresh that changes nothing keeps the list
    // (and which rows are open, and keyboard focus) as it is.
    IReadOnlyList<HistoryRowView>? _rows;
    string? _rowsDay;
    IReadOnlyList<LegendItem>? _legend;

    public HistoryPage(HistoryViewModel history, MainWindow window)
    {
        _history = history;
        _window = window;
        InitializeComponent();
        history.PropertyChanged += OnChanged;
        history.LoadCommand.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName == "IsRunning")
            {
                RetryButton.IsEnabled = !_history.LoadCommand.IsRunning;
            }
        };
        _timer.Tick += (_, _) => _ = _history.RefreshCommand.ExecuteAsync(null);
        Loaded += (_, _) =>
        {
            // Loaded can come twice in a row without Unloaded between: take any earlier subscription off first.
            Show.Changed -= OnBrushesChanged;
            Show.Changed += OnBrushesChanged;
            _ = _history.LoadCommand.ExecuteAsync(null);
            _pageShown = true;
            ApplyGate(askNow: false);
            Render();
        };
        Unloaded += (_, _) =>
        {
            _pageShown = false;
            ApplyGate(askNow: false);
            Show.Changed -= OnBrushesChanged;
        };
        window.ScreenChanged += () => ApplyGate(askNow: true);
        Show.AnnounceChanges(ErrorLine);
        Show.AnnounceChanges(DayLabel);
        Render();
    }

    /// <summary>
    /// Starts or stops the refresh as the page and its window come and go. Back on screen from the tray or from
    /// minimised (<paramref name="askNow"/>), today is asked for at once when the page follows it.
    /// </summary>
    void ApplyGate(bool askNow)
    {
        switch (_gate.Set(_pageShown, _window.IsShown, _window.IsMinimised))
        {
            case RefreshChange.Start:
                _timer.Start();
                if (askNow)
                {
                    _ = _history.RefreshCommand.ExecuteAsync(null);
                }
                break;
            case RefreshChange.Stop:
                _timer.Stop();
                break;
        }
    }

    void OnChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(HistoryViewModel.View) or nameof(CoreViewModel.Error))
        {
            Render();
        }
    }

    void OnBrushesChanged()
    {
        // Dots and the error colour come from the tone brushes: paint the rows again.
        _rows = null;
        _legend = null;
        Render();
    }

    void Render()
    {
        var view = _history.View;
        Show.SetEnabled(PrevButton, view?.PrevDay is not null, NextButton);
        Show.SetEnabled(NextButton, view?.NextDay is not null, PrevButton);
        DayLabel.Text = view?.Label ?? "";

        var error = _history.Error;
        ErrorLine.Text = error ?? "";
        ErrorLine.Foreground = Show.Brush(Tone.Red);
        ErrorRow.Visibility = Show.Visible(error);
        RetryButton.IsEnabled = !_history.LoadCommand.IsRunning;

        // Every refresh brings the same legend: built again only when it changed (the language did).
        var legend = view?.Legend ?? [];
        if (_legend is null || !_legend.SequenceEqual(legend))
        {
            Legend.Children.Clear();
            foreach (var item in legend)
            {
                Legend.Children.Add(LegendItem(item));
            }
            _legend = legend;
        }

        if (view is null)
        {
            DayBlock.Visibility = Visibility.Collapsed;
            return;
        }
        DayBlock.Visibility = Visibility.Visible;
        Strip.Cells = view.Strip;
        Strip.Ticks = view.Ticks;
        Empty.Visibility = Show.Visible(view.Rows.Count == 0);
        if (_rowsDay == view.Day && _rows is not null && SameRows(_rows, view.Rows))
        {
            return;
        }
        // On the same day, rows that were open stay open: counted from the oldest, which a new event never moves.
        var open = new HashSet<int>();
        if (_rowsDay == view.Day && _rows is not null)
        {
            for (var i = 0; i < Rows.Children.Count && i < _rows.Count; i++)
            {
                if (Rows.Children[i] is Expander { IsExpanded: true })
                {
                    open.Add(_rows.Count - 1 - i);
                }
            }
        }
        Rows.Children.Clear();
        for (var i = 0; i < view.Rows.Count; i++)
        {
            Rows.Children.Add(Row(view.Rows[i], i > 0, open.Contains(view.Rows.Count - 1 - i)));
        }
        _rows = view.Rows;
        _rowsDay = view.Day;
    }

    static bool SameRows(IReadOnlyList<HistoryRowView> a, IReadOnlyList<HistoryRowView> b) =>
        a.Count == b.Count
        && a.Zip(b).All(p => p.First.Time == p.Second.Time && p.First.Title == p.Second.Title
            && p.First.Tone == p.Second.Tone && p.First.Details.SequenceEqual(p.Second.Details));

    static UIElement LegendItem(LegendItem item)
    {
        var panel = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 4 };
        panel.Children.Add(Dot(item.Tone));
        panel.Children.Add(Secondary(item.Label, 11));
        return panel;
    }

    static Ellipse Dot(Tone tone)
    {
        var dot = new Ellipse { Width = 8, Height = 8, VerticalAlignment = VerticalAlignment.Center, Fill = Show.Brush(tone) };
        // The row's or legend's words say what the colour means.
        AutomationProperties.SetAccessibilityView(dot, AccessibilityView.Raw);
        return dot;
    }

    /// <summary>
    /// One event (cg-gui event_row): its time, dot and title, with a line above all but the first; with
    /// readings, an expander whose content is the readings in two columns.
    /// </summary>
    static UIElement Row(HistoryRowView row, bool lineAbove, bool open)
    {
        var line = new Grid { ColumnSpacing = 12 };
        line.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(TimeWidth) });
        line.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        line.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        var time = Secondary(row.Time, 13);
        time.FontFamily = new FontFamily("Consolas");
        time.VerticalAlignment = VerticalAlignment.Center;
        var dot = Dot(row.Tone);
        Grid.SetColumn(dot, 1);
        var title = new TextBlock { Text = row.Title, FontSize = 13, TextWrapping = TextWrapping.Wrap, VerticalAlignment = VerticalAlignment.Center };
        Grid.SetColumn(title, 2);
        line.Children.Add(time);
        line.Children.Add(dot);
        line.Children.Add(title);

        var border = lineAbove ? new Thickness(0, 1, 0, 0) : new Thickness(0);
        var divider = (Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"];
        if (row.Details.Count == 0)
        {
            return new Border { Child = line, Padding = new Thickness(14, 10, 14, 10), BorderThickness = border, BorderBrush = divider };
        }
        var expander = new Expander
        {
            Header = line,
            Content = Details(row.Details),
            IsExpanded = open,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            HorizontalContentAlignment = HorizontalAlignment.Stretch,
            BorderThickness = border,
            BorderBrush = divider,
            Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent),
            CornerRadius = new CornerRadius(0),
        };
        // Its time and title, as the row shows them (the header's texts are not read as the expander's name).
        AutomationProperties.SetName(expander, row.Time + " " + row.Title);
        return expander;
    }

    /// <summary>
    /// The readings, two to a line, each its label on the left and its value (monospace) on the right; indented
    /// under the title (the time column, its gap, the dot and its gap).
    /// </summary>
    static UIElement Details(IReadOnlyList<DetailItem> details)
    {
        var grid = new Grid { ColumnSpacing = 24, RowSpacing = 6, Margin = new Thickness(TimeWidth + 32, 0, 0, 2) };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        for (var i = 0; i < details.Count; i++)
        {
            if (i % 2 == 0)
            {
                grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            }
            var cell = new Grid { ColumnSpacing = 8 };
            cell.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            cell.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            var label = Secondary(details[i].Label, 12);
            var value = new TextBlock
            {
                Text = details[i].Value,
                FontSize = 12,
                FontFamily = new FontFamily("Consolas"),
                IsTextSelectionEnabled = true,
            };
            Grid.SetColumn(value, 1);
            cell.Children.Add(label);
            cell.Children.Add(value);
            Grid.SetRow(cell, i / 2);
            Grid.SetColumn(cell, i % 2);
            grid.Children.Add(cell);
        }
        return grid;
    }

    static TextBlock Secondary(string text, double size) => new()
    {
        Text = text,
        FontSize = size,
        TextWrapping = TextWrapping.Wrap,
        Foreground = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"],
    };

    async void OnPrevClick(object sender, RoutedEventArgs e)
    {
        if (_history.View?.PrevDay is { } day)
        {
            await _history.LoadCommand.ExecuteAsync(day);
        }
    }

    async void OnNextClick(object sender, RoutedEventArgs e)
    {
        if (_history.View?.NextDay is { } day)
        {
            await _history.LoadCommand.ExecuteAsync(day);
        }
    }

    async void OnRetryClick(object sender, RoutedEventArgs e) =>
        await _history.LoadCommand.ExecuteAsync(_history.RequestedDay);
}
