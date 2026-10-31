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
        var bad = AppSources.CodeFiles()
            .Where(f => ThemeBrushGuard.IsBad(f.Text, f.File))
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
        // Explicit setters (CaptionTextBlockStyle's size and line height), not BasedOn a deferred system style.
        Assert.DoesNotContain("BasedOn", style);
        Assert.Contains("""<Setter Property="FontSize" Value="12" />""", style);
        Assert.Contains("""<Setter Property="LineHeight" Value="16" />""", style);
        Assert.Contains("""<Setter Property="FontWeight" Value="SemiBold" />""", style);
        Assert.Contains("""<Setter Property="Foreground" Value="{ThemeResource TextFillColorSecondaryBrush}" />""", style);

        var code = File.ReadAllText(Path.Combine(AppSources.AppDir, "Shell", "MainWindow.xaml.cs"));
        var header = System.Text.RegularExpressions.Regex.Match(xaml, """<Style x:Key="SidebarHeader"[\s\S]*?</Style>""").Value;
        Assert.Contains("{StaticResource SidebarHeading}", header);
        Assert.Contains("{TemplateBinding Content}", header);
        Assert.Contains("Style = (Style)Nav.Resources[\"SidebarHeader\"]", code);
        Assert.Contains("IsTabStop = false", code);
        Assert.Contains("AutomationHeadingLevel.Level2", code);
    }

    [Fact]
    public void No_app_style_is_based_on_a_system_style()
    {
        // Deferred system styles can be missing after a theme switch (RowExpander crashed, W3 final review).
        var files = AppSources.XamlFiles().ToList();
        Assert.Contains(files, f => f.File.EndsWith("MainWindow.xaml", StringComparison.Ordinal));
        foreach (var f in files)
        {
            Assert.DoesNotContain("BasedOn=", f.Text);
        }
    }

    [Fact]
    public void The_theme_brush_guard_catches_every_way_of_reading_a_brush()
    {
        foreach (var bad in new[]
        {
            """var b = Application.Current.Resources["TextFillColorPrimaryBrush"] as Brush;""",
            """var b = (SolidColorBrush)Application.Current.Resources["CardStrokeColorDefaultBrush"];""",
            """Application.Current.Resources.TryGetValue("SystemFillColorCautionBrush", out var b);""",
            """var c = (Windows.UI.Color)Resources["SystemAccentColor"];""",
            """var b = Resources["ControlStrokeColorDefaultBrush"];""",
            // A key that is not a literal is judged by what it is read as.
            """var b = (Brush)Application.Current.Resources[key];""",
            """var b = Resources[name] as Microsoft.UI.Xaml.Media.SolidColorBrush;""",
            """if (Resources[name] is Brush b) { }""",
        })
        {
            Assert.True(ThemeBrushGuard.IsBad(bad, file: "X.cs"), bad);
        }
        Assert.False(ThemeBrushGuard.IsBad("""var s = (Style)Nav.Resources["SidebarHeader"];""", file: "MainWindow.xaml.cs"));
        Assert.False(ThemeBrushGuard.IsBad("""Resource("Tone" + tone + "Brush")""", file: "Show.cs"));
        // Show.cs's one reader of the tone brushes, and only there.
        const string reader = "static Brush Resource(string key) => (Brush)Application.Current.Resources[key];";
        Assert.False(ThemeBrushGuard.IsBad(reader, file: Path.Combine("Shell", "Show.cs")));
        Assert.True(ThemeBrushGuard.IsBad(reader, file: "Other.cs"));
        // Show.cs is still checked for everything else.
        Assert.True(ThemeBrushGuard.IsBad(reader + "\nvar b = Resources[\"TextFillColorPrimaryBrush\"];", file: Path.Combine("Shell", "Show.cs")));
    }

    /// <summary>
    /// What Code_takes_theme_colours_from_styles_not_resource_brushes refuses: a resource key ending in Brush or
    /// Color (by index or TryGetValue), or a resource read cast to, or taken as, a Brush, SolidColorBrush or Color.
    /// Show.cs's Resource helper (the tone and low-power brushes, the same in light and dark) is the one exception.
    /// </summary>
    static class ThemeBrushGuard
    {
        const string ShowReader = "(Brush)Application.Current.Resources[key]";
        const string Brushy = @"(?:[\w.]+\.)?(?:Brush|SolidColorBrush|Color)\b";

        static readonly System.Text.RegularExpressions.Regex[] Rules =
        [
            // A theme key, however it is read.
            new(@"(?:Resources\s*\[|TryGetValue\s*\()\s*@?""[^""]*(?:Brush|Color)""\s*[\],]"),
            // (Brush)...Resources[...], (Windows.UI.Color)Resources[...]
            new(@"\(\s*" + Brushy + @"\s*\)\s*(?:[\w.]+\.)?Resources\b"),
            // Resources[...] as Brush, Resources[...] is SolidColorBrush b
            new(@"Resources\s*\[[^\]]*\]\s*(?:as|is)\s+" + Brushy),
        ];

        public static bool IsBad(string text, string file)
        {
            if (Path.GetFileName(file) == "Show.cs")
            {
                text = text.Replace(ShowReader, "", StringComparison.Ordinal);
            }
            return Rules.Any(rule => rule.IsMatch(text));
        }
    }

    /// <summary>
    /// Decision 12: the keyboard reaches the panel from the focused tray icon. H.NotifyIcon 2.4.1 acts only on the
    /// mouse's button-up messages; its message window reports NIN_SELECT (0x400), NIN_KEYSELECT (0x401) and
    /// WM_CONTEXTMENU (0x007B) as KeyboardEventReceived, which TrayHost passes to the panel's toggle unless a click
    /// just came (TrayKeyboard; Windows 11 makes clicks of Enter, Space and Shift+F10, which the library handles).
    /// TrayHost watches both buttons going down and up. The panel then gives the keyboard focus to its first control.
    /// </summary>
    [Fact]
    public void Tray_opens_the_panel_from_the_keyboard()
    {
        var tray = File.ReadAllText(Path.Combine(AppSources.AppDir, "Tray", "TrayHost.cs"));
        Assert.Contains("MessageWindow.KeyboardEventReceived += OnKeyboard", tray);
        Assert.Contains("MessageWindow.KeyboardEventReceived -= OnKeyboard", tray);
        Assert.Contains("MessageWindow.MouseEventReceived += OnMouse", tray);
        foreach (var button in new[] { "IconLeftMouseDown", "IconLeftMouseUp", "IconRightMouseDown", "IconRightMouseUp" })
        {
            Assert.Contains("MouseEvent." + button, tray);
        }
        var handler = System.Text.RegularExpressions.Regex.Match(tray, @"void OnKeyboard\([\s\S]*?\n    }").Value;
        foreach (var key in new[] { "KeyboardEvent.Select => TrayKey.Select", "KeyboardEvent.KeySelect => TrayKey.KeySelect", "KeyboardEvent.ContextMenu => TrayKey.ContextMenu" })
        {
            Assert.Contains(key, handler);
        }
        Assert.Contains("Post(() => _togglePanel(true))", handler);
        Assert.Contains("_keys.Toggles(", handler);

        var panel = File.ReadAllText(Path.Combine(AppSources.AppDir, "Tray", "PanelWindow.xaml.cs"));
        Assert.Contains("FocusManager.FindFirstFocusableElement(Root)", panel);
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
