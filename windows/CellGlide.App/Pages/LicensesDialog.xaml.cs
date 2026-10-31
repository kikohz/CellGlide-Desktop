using CellGlide.Core.Desktop;
using Microsoft.UI.Xaml.Controls;

namespace CellGlide.App.Pages;

/// <summary>The licenses list: <see cref="LicenseText.Paragraphs"/> of the DLL's text, a row each, nothing added.</summary>
public sealed partial class LicensesDialog : ContentDialog
{
    public LicensesDialog(string licenses)
    {
        InitializeComponent();
        // A List<string> written out: CsWinRT hands a concrete list to XAML (see DiagnosticsPage's file types).
        List.ItemsSource = new List<string>(LicenseText.Paragraphs(licenses));
    }
}
