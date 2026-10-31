using System.ComponentModel;
using CellGlide.App.Hosting;
using CellGlide.App.Shell;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;
using CommunityToolkit.Mvvm.Input;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace CellGlide.App.Controls;

/// <summary>
/// The doctor's checks as cg-gui's widgets/checks.rs shows them: a button that runs them (it says
/// <c>doctor.running</c> while they run), the reason in red when they could not be had, then a row per
/// check: a green ✓ for Pass, a red ✗ for Fail, an orange ? for Unknown (it couldn't be checked: not a
/// pass, not known to fail), the DLL's line, and under it the fix with a Copy button.
/// <para>
/// "Copied" is this control's own short-lived state: after a copy that fix's button says
/// <c>common.copied</c> for 2 seconds. It is feedback only, so it never goes to the DLL and back. The
/// DLL's <see cref="CheckRow.Copied"/> serves the Linux window; on Windows the DLL always sends false,
/// and this control never reads it.
/// </para>
/// The marks are not read out (AccessibilityView Raw): the line itself says, in words, whether the
/// check passed, failed or couldn't be made.
/// </summary>
public sealed partial class ChecksList : UserControl
{
    public static readonly DependencyProperty ChecksProperty = DependencyProperty.Register(
        nameof(Checks), typeof(object), typeof(ChecksList), new PropertyMetadata(null, (d, e) => ((ChecksList)d).Attach(e.OldValue as ChecksViewModel)));

    public static readonly DependencyProperty RunLabelProperty = DependencyProperty.Register(
        nameof(RunLabel), typeof(string), typeof(ChecksList), new PropertyMetadata("", (d, _) => ((ChecksList)d).ShowRunButton()));

    /// <summary>Each shown fix and its Copy button, to relabel in place (keyboard focus stays put).</summary>
    readonly List<(string Fix, Button Button)> _copyButtons = [];
    /// <summary>The fix just copied, while its button says so.</summary>
    readonly CopyFeedback _copy;
    bool _attached;

    public ChecksList()
    {
        _copy = new CopyFeedback(Relabel);
        InitializeComponent();
        Loaded += (_, _) =>
        {
            // Loaded can come twice in a row without Unloaded between: take any earlier subscription off first,
            // so there is never more than one.
            Show.Changed -= Repaint;
            Show.Changed += Repaint;
            Attach(null);
        };
        Unloaded += (_, _) =>
        {
            Show.Changed -= Repaint;
            Detach(Checks);
        };
        Show.AnnounceChanges(ErrorLine);
        ShowRunButton();
    }

    /// <summary>The run button, for a page that hands keyboard focus to it.</summary>
    public Control RunControl => RunButton;

    /// <summary>The checklist to run and show (a <see cref="ChecksViewModel"/>).</summary>
    public ChecksViewModel? Checks
    {
        get => (ChecksViewModel?)GetValue(ChecksProperty);
        set => SetValue(ChecksProperty, value);
    }

    /// <summary>The run button's text (a DLL text, e.g. <c>leak.run_checks</c>).</summary>
    public string RunLabel
    {
        get => (string)GetValue(RunLabelProperty);
        set => SetValue(RunLabelProperty, value);
    }

    void Attach(ChecksViewModel? old)
    {
        if (old is not null)
        {
            Detach(old);
        }
        // Loaded can come twice in a row: subscribe once.
        if (!_attached && Checks is { } checks && IsLoaded)
        {
            checks.PropertyChanged += OnChecksChanged;
            checks.RunCommand.PropertyChanged += OnRunChanged;
            _attached = true;
        }
        ShowRunButton();
        ShowError();
        ShowRows();
    }

    void Detach(ChecksViewModel? checks)
    {
        if (checks is not null && _attached)
        {
            checks.PropertyChanged -= OnChecksChanged;
            checks.RunCommand.PropertyChanged -= OnRunChanged;
        }
        _attached = false;
    }

    void OnChecksChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(ChecksViewModel.Rows))
        {
            ShowRows();
        }
        else if (e.PropertyName == nameof(ChecksViewModel.Error))
        {
            ShowError();
        }
    }

    void OnRunChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(IAsyncRelayCommand.IsRunning))
        {
            ShowRunButton();
        }
    }

    async void OnRunClick(object sender, RoutedEventArgs e)
    {
        if (Checks is { } checks && !checks.RunCommand.IsRunning)
        {
            // A failed run leaves its sentence in Error; nothing is thrown here.
            await checks.RunCommand.ExecuteAsync(null);
        }
    }

    void ShowRunButton()
    {
        var running = Checks?.RunCommand.IsRunning == true;
        RunButton.Content = running ? L.T("doctor.running") : RunLabel;
        // Off while running: focus waits on a Copy button (or nowhere) and comes back when the run is done.
        Show.SetEnabled(RunButton, Checks is not null && !running, [.. _copyButtons.Select(b => b.Button)]);
    }

    void ShowError()
    {
        var error = Checks?.Error;
        ErrorLine.Text = error ?? "";
        ErrorLine.Foreground = Show.Brush(Tone.Red);
        ErrorLine.Visibility = Show.Visible(error);
    }

    void ShowRows()
    {
        Rows.Children.Clear();
        _copyButtons.Clear();
        foreach (var row in Checks?.Rows ?? [])
        {
            Rows.Children.Add(Row(row));
        }
    }

    UIElement Row(CheckRow row)
    {
        var (mark, tone) = row.State switch
        {
            CheckState.Pass => ("✓", Tone.Green),
            CheckState.Fail => ("✗", Tone.Red),
            _ => ("?", Tone.Orange),
        };
        var head = new Grid { ColumnSpacing = 8 };
        head.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        head.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        var markText = new TextBlock { Text = mark, FontSize = 13, FontWeight = FontWeights.SemiBold, Foreground = Show.Brush(tone) };
        AutomationProperties.SetAccessibilityView(markText, AccessibilityView.Raw);
        var line = new TextBlock { Text = row.Line, FontSize = 13, TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true };
        Grid.SetColumn(line, 1);
        head.Children.Add(markText);
        head.Children.Add(line);

        var entry = new StackPanel { Spacing = 4 };
        entry.Children.Add(head);
        if (row.Fix is { } fix)
        {
            var fixRow = new Grid { ColumnSpacing = 8, Margin = new Thickness(21, 0, 0, 0) };
            fixRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            fixRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            var shown = new TextBlock
            {
                Text = L.T("check.fix", "fix", fix),
                FontFamily = new FontFamily("Consolas"),
                FontSize = 12,
                TextWrapping = TextWrapping.Wrap,
                IsTextSelectionEnabled = true,
                VerticalAlignment = VerticalAlignment.Center,
            };
            var copy = new Button { Padding = new Thickness(8, 2, 8, 3), VerticalAlignment = VerticalAlignment.Center };
            // The button's name is its own text; LabeledBy ties it to the fix it copies.
            AutomationProperties.SetLabeledBy(copy, shown);
            AutomationProperties.SetLiveSetting(copy, AutomationLiveSetting.Polite);
            copy.Click += (_, _) => _copy.Copy(fix, copy);
            Grid.SetColumn(copy, 1);
            fixRow.Children.Add(shown);
            fixRow.Children.Add(copy);
            entry.Children.Add(fixRow);
            _copyButtons.Add((fix, copy));
            Label(fix, copy);
        }
        return entry;
    }

    void Label(string fix, Button button) => CopyFeedback.SetLabel(button, _copy.IsCopied(fix));

    void Relabel()
    {
        foreach (var (fix, button) in _copyButtons)
        {
            Label(fix, button);
        }
    }

    void Repaint()
    {
        ShowError();
        ShowRows();
    }
}
