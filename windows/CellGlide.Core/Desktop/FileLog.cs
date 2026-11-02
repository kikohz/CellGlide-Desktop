using System.Diagnostics;
using System.Globalization;
using System.Text;

namespace CellGlide.Core.Desktop;

/// <summary>
/// The app's own log (for now only <see cref="AppleDevicesStarter"/> writes to it): one timestamped line per
/// call, appended to <c>path</c>. Once the file passes <c>maxBytes</c> it becomes <c>path.1</c> (replacing the
/// one before) and a new file begins, so the two never hold much more than twice that. Any thread may write;
/// a failure to write goes to the debugger only.
/// </summary>
public sealed class FileLog(string path, long maxBytes = 256 * 1024)
{
    readonly Lock _gate = new();

    public string Path => path;

    public void Write(string line)
    {
        var text = DateTimeOffset.Now.ToString("yyyy-MM-dd'T'HH:mm:ss.fffzzz", CultureInfo.InvariantCulture) + " " + line + "\n";
        lock (_gate)
        {
            try
            {
                Directory.CreateDirectory(System.IO.Path.GetDirectoryName(path)!);
                var file = new FileInfo(path);
                if (file.Exists && file.Length >= maxBytes)
                {
                    File.Move(path, path + ".1", overwrite: true);
                }
                File.AppendAllText(path, text, new UTF8Encoding(false));
            }
            catch (Exception e)
            {
                Debug.WriteLine("CellGlide: log " + path + ": " + e.Message + ": " + line);
            }
        }
    }
}
