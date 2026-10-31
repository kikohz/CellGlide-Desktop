using CellGlide.Core.Desktop;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace CellGlide.App.Pages;

/// <summary>The licenses list: <see cref="LicenseText.Paragraphs"/> of the DLL's text, a row each, nothing added.</summary>
public sealed partial class LicensesDialog : ContentDialog
{
    /// <summary>The list's widest.</summary>
    const double MaxListWidth = 700;
    /// <summary>The narrowest the list goes (the dialog's own minimum is about this plus its padding).</summary>
    const double MinListWidth = 240;
    /// <summary>What the dialog adds around its content (padding and border) plus a gap to the window's edges.</summary>
    const double Around = 2 * (24 + 1) + 2 * 24;

    public LicensesDialog(string licenses)
    {
        InitializeComponent();
        // A List<string> written out: CsWinRT hands a concrete list to XAML (see DiagnosticsPage's file types).
        List.ItemsSource = new List<string>(LicenseText.Paragraphs(licenses));
        // XamlRoot is set after construction: size before the first layout, and again whenever the window's size changes.
        Loading += (_, _) =>
        {
            if (XamlRoot is { } root)
            {
                root.Changed -= OnRootChanged;
                root.Changed += OnRootChanged;
                Fit(root);
            }
        };
        Closed += (_, _) =>
        {
            if (XamlRoot is { } root)
            {
                root.Changed -= OnRootChanged;
            }
        };
    }

    void OnRootChanged(XamlRoot root, XamlRootChangedEventArgs e) => Fit(root);

    void Fit(XamlRoot root) => List.Width = Math.Clamp(root.Size.Width - Around, MinListWidth, MaxListWidth);
}
