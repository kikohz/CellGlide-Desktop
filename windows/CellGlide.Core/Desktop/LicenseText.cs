namespace CellGlide.Core.Desktop;

/// <summary>
/// The About page's licenses (the DLL's <c>AboutValue.Licenses</c>) cut into paragraphs, only so the
/// list can be virtualized: some 740 KB in a single text element would be laid out all at once. The
/// text itself is shown exactly as the DLL gives it.
/// </summary>
public static class LicenseText
{
    /// <summary>
    /// Splits on blank lines (a line of only spaces counts as blank), leaves out the blank lines at the
    /// start and end, and keeps the line breaks and indentation inside each paragraph. A CR before a
    /// line break is dropped.
    /// </summary>
    public static IReadOnlyList<string> Paragraphs(string text)
    {
        var paragraphs = new List<string>();
        var lines = new List<string>();
        foreach (var raw in text.Split('\n'))
        {
            var line = raw.EndsWith('\r') ? raw[..^1] : raw;
            if (string.IsNullOrWhiteSpace(line))
            {
                Flush();
            }
            else
            {
                lines.Add(line);
            }
        }
        Flush();
        return paragraphs;

        void Flush()
        {
            if (lines.Count > 0)
            {
                paragraphs.Add(string.Join('\n', lines));
                lines.Clear();
            }
        }
    }
}
