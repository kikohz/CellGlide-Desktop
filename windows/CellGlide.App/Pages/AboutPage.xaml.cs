using System.ComponentModel;
using System.Diagnostics;
using CellGlide.App.Hosting;
using CellGlide.App.Shell;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.System;

namespace CellGlide.App.Pages;

/// <summary>
/// About (cg-gui pages/about.rs) over <see cref="AboutViewModel"/>: asks the DLL the first time the page shows
/// (and again only if that failed) for the version, the Telegram link and the licenses, and shows them as they
/// are. Telegram opens the DLL's link in the default browser; View opens <see cref="LicensesDialog"/>; Quit is
/// the app's own quit (tray icon away, windows hidden, tests cancelled and the DLL stopped, then exit).
/// </summary>
public sealed partial class AboutPage : Page
{
    readonly AboutViewModel _about;
    readonly CopyFeedback _copy;
    bool _dialogOpen;

    public AboutPage(AboutViewModel about)
    {
        _about = about;
        _copy = new CopyFeedback(Render);
        InitializeComponent();
        Icon.Source = new BitmapImage(new Uri(Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon-128.png")));
        about.PropertyChanged += OnChanged;
        about.LoadCommand.PropertyChanged += OnChanged;
        Loaded += (_, _) =>
        {
            Show.Changed -= Render;
            Show.Changed += Render;
            // The first time the page shows (and again after a failed try).
            if (_about.About is null && !_about.LoadCommand.IsRunning)
            {
                _ = _about.LoadCommand.ExecuteAsync(null);
            }
            Render();
        };
        Unloaded += (_, _) => Show.Changed -= Render;
        Show.AnnounceChanges(ErrorLine);
        Render();
    }

    void OnChanged(object? sender, PropertyChangedEventArgs e) => Render();

    void Render()
    {
        var about = _about.About;
        ErrorLine.Text = _about.Error ?? "";
        ErrorLine.Foreground = Show.Brush(Tone.Red);
        ErrorLine.Visibility = Show.Visible(_about.Error);

        VersionText.Text = about is null ? "" : L.T("about.version", "version", about.Version);
        CopyFeedback.SetLabel(CopyVersionButton, _copy.Copied is not null);
        AutomationProperties.SetHelpText(CopyVersionButton, VersionText.Text);

        TelegramButton.Content = about?.TelegramUrl ?? "";
        AutomationProperties.SetHelpText(TelegramButton, about?.TelegramUrl ?? "");
        Show.SetEnabled(
            [(CopyVersionButton, about is not null), (TelegramButton, about is not null), (ShowLicensesButton, about is not null && !_dialogOpen)],
            [CopyVersionButton, TelegramButton, ShowLicensesButton, QuitButton]);
    }

    void OnCopyVersionClick(object sender, RoutedEventArgs e)
    {
        if (_about.About is { } about)
        {
            _copy.Copy(about.Version, CopyVersionButton);
        }
    }

    /// <summary>The DLL's link, in the default browser.</summary>
    async void OnTelegramClick(object sender, RoutedEventArgs e)
    {
        if (_about.About is not { } about || !Uri.TryCreate(about.TelegramUrl, UriKind.Absolute, out var uri))
        {
            return;
        }
        try
        {
            await Launcher.LaunchUriAsync(uri);
        }
        catch (Exception ex)
        {
            Debug.WriteLine("CellGlide: opening the Telegram link failed: " + ex);
        }
    }

    async void OnShowLicensesClick(object sender, RoutedEventArgs e)
    {
        if (_about.About is not { } about || _dialogOpen)
        {
            return;
        }
        _dialogOpen = true;
        Render();
        try
        {
            await new LicensesDialog(about.Licenses) { XamlRoot = XamlRoot }.ShowAsync();
        }
        catch (Exception ex)
        {
            // Another dialog is up: nothing is shown.
            Debug.WriteLine("CellGlide: the licenses dialog failed: " + ex);
        }
        finally
        {
            _dialogOpen = false;
            Render();
            ShowLicensesButton.Focus(FocusState.Programmatic);
        }
    }

    /// <summary>The app's own quit, as the tray's Quit does it.</summary>
    void OnQuitClick(object sender, RoutedEventArgs e) => _ = App.Current.QuitAsync();
}
