using System.Reflection;
using CellGlide.Core.ViewModels;

namespace CellGlide.Tests;

/// <summary>
/// x:Bind does not call a function again when the path of one of its arguments runs into null (the
/// generated code checks each step), so the App's pages must pass functions values that are never
/// null: <c>Shell.Now</c>, not <c>Shell.Overview</c>.
/// </summary>
public class XBindTests
{
    [Fact]
    public void No_function_binding_takes_a_path_that_can_be_null()
    {
        var bad = AppSources.BindCalls()
            .SelectMany(call => call.Args
                .Where(CanBeNull)
                .Select(arg => $"{call.File}: {call.Function}({arg})"))
            .ToList();
        Assert.Empty(bad);
    }

    /// <summary>
    /// A control that goes off while it has keyboard focus leaves the user nowhere unless focus is moved: pages
    /// turn controls on and off only through Show.SetEnabled (Show.cs itself, and the tray menu's items, which
    /// are not focusable XAML controls, are the exceptions).
    /// </summary>
    [Fact]
    public void Pages_turn_controls_off_only_through_show_set_enabled()
    {
        var raw = new System.Text.RegularExpressions.Regex(@"\.IsEnabled\s*=[^=]");
        var bad = AppSources.CodeFiles()
            .Where(f => !f.File.EndsWith("Show.cs", StringComparison.Ordinal) && !f.File.EndsWith("TrayHost.cs", StringComparison.Ordinal))
            .Where(f => raw.IsMatch(f.Text))
            .Select(f => f.File)
            .ToList();
        Assert.Empty(bad);
    }

    /// <summary>
    /// W3 final review M1: a Command binding turns its button off while the command runs with the keyboard focus
    /// still on it, so the connect buttons (overview and panel) click instead and go off through Show.SetEnabled
    /// while ToggleCommand.IsRunning, handing focus to Change... / Open CellGlide and taking it back afterwards.
    /// </summary>
    [Fact]
    public void Connect_buttons_go_off_through_show_set_enabled()
    {
        foreach (var (xamlFile, button, fallback) in new[]
        {
            (Path.Combine("Pages", "OverviewPage.xaml"), "ToggleButton", "ChangeButton"),
            (Path.Combine("Tray", "PanelWindow.xaml"), "MainButton", "OpenButton"),
        })
        {
            var xaml = File.ReadAllText(Path.Combine(AppSources.AppDir, xamlFile));
            Assert.DoesNotContain("Command=\"{x:Bind Shell.ToggleCommand}\"", xaml);
            Assert.Contains($"x:Name=\"{button}\"", xaml);
            Assert.Contains($"x:Name=\"{fallback}\"", xaml);
            var code = File.ReadAllText(Path.Combine(AppSources.AppDir, xamlFile + ".cs"));
            Assert.Contains($"Show.SetEnabled({button}, !Shell.ToggleCommand.IsRunning, {fallback})", code);
            Assert.Contains("ToggleCommand.PropertyChanged +=", code);
            Assert.Contains("Shell.ToggleCommand.CanExecute(null)", code);
        }
    }

    /// <summary>
    /// W3 final review M2: Loaded can come twice without an Unloaded between, so every page and control that
    /// subscribes a method to Show.Changed takes it off first (Unloaded takes off only one). The windows'
    /// lambdas live as long as the app and subscribe once, in their constructors.
    /// </summary>
    [Fact]
    public void Show_changed_is_unsubscribed_before_it_is_subscribed()
    {
        var add = new System.Text.RegularExpressions.Regex(@"Show\.Changed\s*\+=\s*(\w+)\s*;");
        var bad = new List<string>();
        var count = 0;
        foreach (var (file, text) in AppSources.CodeFiles())
        {
            foreach (System.Text.RegularExpressions.Match m in add.Matches(text))
            {
                count++;
                var before = text[..m.Index].TrimEnd();
                if (!before.EndsWith("Show.Changed -= " + m.Groups[1].Value + ";", StringComparison.Ordinal))
                {
                    bad.Add($"{file}: {m.Value}");
                }
            }
        }
        Assert.Empty(bad);
        Assert.True(count >= 11, $"only {count} subscriptions found");
    }

    /// <summary>
    /// W3 final review I2: a brush taken from Application.Current.Resources in code is a fixed instance of the theme
    /// the app started in, so text painted with it stays dark-theme grey after a switch to light. Code gives theme
    /// colours through App.xaml's styles (Show.ThemeStyle), whose ThemeResource setters follow the switch. Only
    /// Show.cs reads brushes from the resources, and only the tone brushes, which are the same in light and dark.
    /// </summary>
    [Fact]
    public void Code_takes_theme_colours_from_styles_not_resource_brushes()
    {
        var themeBrush = new System.Text.RegularExpressions.Regex(@"Resources\[\s*""(TextFill|DividerStroke|CardStroke|ControlFill|SolidBackground|LayerFill|CardBackground|AccentFill|AccentTextFill)");
        var brushCast = new System.Text.RegularExpressions.Regex(@"\(\s*(Microsoft\.UI\.Xaml\.Media\.)?Brush\s*\)\s*Application\.Current\.Resources");
        var bad = AppSources.CodeFiles()
            .Where(f => themeBrush.IsMatch(f.Text) || (brushCast.IsMatch(f.Text) && !f.File.EndsWith("Show.cs", StringComparison.Ordinal)))
            .Select(f => f.File)
            .ToList();
        Assert.Empty(bad);

        var show = File.ReadAllText(Path.Combine(AppSources.AppDir, "Shell", "Show.cs"));
        Assert.Contains("Resource(\"Tone\" + tone + \"Brush\")", show);
        Assert.Contains("Resource(\"LowPowerBrush\")", show);
        Assert.Equal(3, System.Text.RegularExpressions.Regex.Matches(show, @"\bResource\(").Count);

        var app = File.ReadAllText(Path.Combine(AppSources.AppDir, "App.xaml"));
        foreach (var (key, brush) in new[]
        {
            ("SecondaryText", "TextFillColorSecondaryBrush"),
            ("PrimaryText", "TextFillColorPrimaryBrush"),
            ("DividerRule", "DividerStrokeColorDefaultBrush"),
            ("RowBorder", "CardStrokeColorDefaultBrush"),
            ("RowExpander", "CardStrokeColorDefaultBrush"),
        })
        {
            var style = System.Text.RegularExpressions.Regex.Match(app, $"""<Style x:Key="{key}"[\s\S]*?</Style>""").Value;
            Assert.Contains("{ThemeResource " + brush + "}", style);
        }
    }

    /// <summary>
    /// Checklist e (W3 Task 10, user-reported): the sidebar's group headings must read as headings, not as
    /// items. The stock NavigationViewItemHeader draws its text like the items under it; the header gets the
    /// SidebarHeader style, whose template shows the DLL's heading with SidebarHeading (caption size, semibold,
    /// secondary colour); the header is no tab stop and is announced as a heading.
    /// </summary>
    [Fact]
    public void Sidebar_headings_use_the_heading_style()
    {
        var xaml = File.ReadAllText(Path.Combine(AppSources.AppDir, "Shell", "MainWindow.xaml"));
        var style = System.Text.RegularExpressions.Regex.Match(xaml, """<Style x:Key="SidebarHeading"[\s\S]*?</Style>""").Value;
        Assert.Contains("BasedOn=\"{StaticResource CaptionTextBlockStyle}\"", style);
        Assert.Contains("SemiBold", style);
        Assert.Contains("{ThemeResource TextFillColorSecondaryBrush}", style);

        var code = File.ReadAllText(Path.Combine(AppSources.AppDir, "Shell", "MainWindow.xaml.cs"));
        var header = System.Text.RegularExpressions.Regex.Match(xaml, """<Style x:Key="SidebarHeader"[\s\S]*?</Style>""").Value;
        Assert.Contains("{StaticResource SidebarHeading}", header);
        Assert.Contains("{TemplateBinding Content}", header);
        Assert.Contains("Style = (Style)Nav.Resources[\"SidebarHeader\"]", code);
        Assert.Contains("IsTabStop = false", code);
        Assert.Contains("AutomationHeadingLevel.Level2", code);
    }

    [Fact]
    public void A_nullable_shell_path_is_caught()
    {
        var calls = AppSources.BindCalls(
            """<TextBlock Text="{x:Bind s:Show.Title(Shell.Overview), Mode=OneWay}" Visibility="{x:Bind Show.Visible(Shell.Chrome.Menu)}" Tag="{x:Bind F(Shell.Now, 'x', Shell.ToggleCommand)}" />""")
            .ToList();
        Assert.Equal(3, calls.Count);
        Assert.Equal(["Shell.Overview"], calls[0].Args.Where(CanBeNull));
        Assert.Equal(["Shell.Chrome.Menu"], calls[1].Args.Where(CanBeNull));
        Assert.DoesNotContain(calls[2].Args, CanBeNull);
        Assert.Contains(AppSources.BindCalls(), call => call.Args.Contains("Shell.Now"));
    }

    static readonly NullabilityInfoContext Nullability = new();

    /// <summary>
    /// Whether an argument path through ShellViewModel meets a property declared nullable (Overview,
    /// ServiceDown, Chrome, Error...), or an unknown member. Other roots are not judged.
    /// </summary>
    static bool CanBeNull(string arg)
    {
        var parts = arg.Split('.');
        if (parts[0] != "Shell" || parts.Length < 2)
        {
            return false;
        }
        var type = typeof(ShellViewModel);
        foreach (var name in parts.Skip(1))
        {
            if (type.GetProperty(name, BindingFlags.Public | BindingFlags.Instance) is not { } property)
            {
                return true;
            }
            if (!property.PropertyType.IsValueType && Nullability.Create(property).ReadState != NullabilityState.NotNull)
            {
                return true;
            }
            type = property.PropertyType;
        }
        return false;
    }
}
