using System.ComponentModel;
using CellGlide.App.Hosting;
using CellGlide.App.Shell;
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
/// The DNS leak test (cg-gui pages/leak.rs) drawn from <see cref="LeakViewModel.View"/>, filled in code
/// each time the view changes (x:Bind would stop calling a function whose argument path is null). The
/// verdict's tone, title and sentence, your exit, the groups and the subnet line are the DLL's; the code
/// only lays them out, and puts each group's query count into the DLL's <c>leak.queries</c>.
/// </summary>
public sealed partial class LeakPage : Page
{
    readonly LeakViewModel _leak;

    public LeakPage(LeakViewModel leak)
    {
        _leak = leak;
        InitializeComponent();
        DoctorChecks.Checks = leak.Checks;
        DoctorChecks.RunLabel = L.T("leak.run_checks");
        leak.PropertyChanged += OnChanged;
        Loaded += (_, _) =>
        {
            // Loaded can come twice in a row without Unloaded between: take any earlier subscription off first,
            // so there is never more than one.
            Show.Changed -= Render;
            Show.Changed += Render;
            Render();
        };
        Unloaded += (_, _) => Show.Changed -= Render;
        Show.AnnounceChanges(ErrorLine);
        Show.AnnounceChanges(VerdictTitle);
        Render();
    }

    void OnChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(LeakViewModel.View) or nameof(CoreViewModel.Error))
        {
            Render();
        }
    }

    void Render()
    {
        var view = _leak.View;
        var running = view?.Running == true;
        // Before the DLL's first view the button waits, with the label it starts with.
        StartButton.Content = view?.StartLabel ?? L.T("leak.start");
        StartButton.IsEnabled = view is not null && !running;
        RunningLine.Visibility = Show.Visible(running);

        var error = view?.Error ?? _leak.Error;
        ErrorLine.Text = error ?? "";
        ErrorLine.Foreground = Show.Brush(Tone.Red);
        ErrorLine.Visibility = Show.Visible(error);

        if (view?.Result is not { } result)
        {
            ResultBlock.Visibility = Visibility.Collapsed;
            return;
        }
        ResultBlock.Visibility = Visibility.Visible;
        var tone = Show.Brush(result.Tone);
        ToneDot.Fill = tone;
        VerdictTitle.Text = result.Title;
        VerdictTitle.Foreground = tone;
        VerdictDetail.Text = result.Detail;
        VerdictCard.Background = Tint(tone);

        DoctorChecks.Visibility = Show.Visible(view.OffersChecks);

        You.Text = result.You;
        NoneSeen.Visibility = Show.Visible(result.Groups.Count == 0);
        Groups.Children.Clear();
        foreach (var group in result.Groups)
        {
            Groups.Children.Add(Group(group, result.Subnet));
        }
        Subnet.Text = result.Subnet ?? "";
        Subnet.Visibility = Show.Visible(result.Subnet);
    }

    /// <summary>The verdict card's background: its tone at 12% (cg-gui verdict_block); none under high contrast's plain brushes.</summary>
    static Brush? Tint(Brush tone) =>
        tone is SolidColorBrush solid ? new SolidColorBrush(solid.Color) { Opacity = 0.12 } : null;

    /// <summary>
    /// One owner of DNS servers (cg-gui servers_card): its name, with an orange dot when it passes on
    /// the client subnet (named by the DLL's subnet line), and its query count under it; its addresses;
    /// its location.
    /// </summary>
    static UIElement Group(ResolverGroupView group, string? subnet)
    {
        var row = new Grid { ColumnSpacing = 12 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(3, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(4, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(2, GridUnitType.Star) });

        var who = new StackPanel { Spacing = 2 };
        var name = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        name.Children.Add(new TextBlock { Text = group.Name, FontSize = 13, TextWrapping = TextWrapping.Wrap });
        if (group.Ecs)
        {
            var dot = new Ellipse { Width = 7, Height = 7, VerticalAlignment = VerticalAlignment.Center, Fill = Show.Brush(Tone.Orange) };
            if (subnet is not null)
            {
                AutomationProperties.SetName(dot, subnet);
                ToolTipService.SetToolTip(dot, subnet);
            }
            else
            {
                AutomationProperties.SetAccessibilityView(dot, AccessibilityView.Raw);
            }
            name.Children.Add(dot);
        }
        who.Children.Add(name);
        if (group.Queries is { } queries)
        {
            who.Children.Add(Secondary(L.T("leak.queries", "count", queries.ToString(System.Globalization.CultureInfo.InvariantCulture)), 11));
        }

        var addresses = new TextBlock
        {
            Text = string.Join("\n", group.Addresses),
            FontFamily = new FontFamily("Consolas"),
            FontSize = 12,
            IsTextSelectionEnabled = true,
            TextWrapping = TextWrapping.Wrap,
        };
        Grid.SetColumn(addresses, 1);
        var location = Secondary(group.Location ?? "", 12);
        location.TextAlignment = TextAlignment.Right;
        Grid.SetColumn(location, 2);

        row.Children.Add(who);
        row.Children.Add(addresses);
        row.Children.Add(location);
        return row;
    }

    static TextBlock Secondary(string text, double size) => new()
    {
        Text = text,
        FontSize = size,
        TextWrapping = TextWrapping.Wrap,
        Foreground = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"],
    };

    async void OnStartClick(object sender, RoutedEventArgs e)
    {
        if (_leak.View is { Running: false })
        {
            // A failed start leaves its sentence in Error, shown above.
            await _leak.StartCommand.ExecuteAsync(null);
        }
    }
}
