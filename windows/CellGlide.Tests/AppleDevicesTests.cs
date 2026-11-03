using CellGlide.Core.Desktop;

namespace CellGlide.Tests;

/// <summary>
/// Starting Apple Devices: the pace (the first attempt 30 s into an episode, then once a minute, across episodes
/// too, three times an episode and six an hour), the button (usbmuxd first, then the window), the log, and
/// failures kept in.
/// </summary>
public class AppleDevicesTests
{
    /// <summary>Counts the calls, in order; each answers with what <see cref="Next"/> says.</summary>
    sealed class FakeLauncher : IAppleDevicesLauncher
    {
        public int Started { get; private set; }

        public int Opened { get; private set; }

        /// <summary>"start", "open" and "listening?" as they were called.</summary>
        public List<string> Calls { get; } = [];

        /// <summary>What the next call does: complete (null), throw at once, or return this task.</summary>
        public Func<Task>? Next { get; set; }

        /// <summary>What <see cref="UsbmuxdListening"/> answers.</summary>
        public Func<bool> Listening { get; set; } = () => false;

        public Task StartInBackgroundAsync()
        {
            Started++;
            Calls.Add("start");
            return Next?.Invoke() ?? Task.CompletedTask;
        }

        public Task OpenAsync()
        {
            Opened++;
            Calls.Add("open");
            return Next?.Invoke() ?? Task.CompletedTask;
        }

        public bool UsbmuxdListening()
        {
            Calls.Add("listening?");
            return Listening();
        }
    }

    sealed class Clock
    {
        public DateTimeOffset Now { get; set; } = new(2026, 11, 2, 12, 0, 0, TimeSpan.Zero);

        public void Add(int seconds) => Now = Now.AddSeconds(seconds);
    }

    static (AppleDevicesStarter Starter, FakeLauncher Launcher, Clock Clock, List<string> Log) Make(List<TimeSpan>? waits = null)
    {
        var launcher = new FakeLauncher();
        var clock = new Clock();
        var log = new List<string>();
        Func<TimeSpan, Task> delay = d =>
        {
            waits?.Add(d);
            clock.Add((int)Math.Ceiling(d.TotalSeconds));
            return Task.CompletedTask;
        };
        return (new AppleDevicesStarter(launcher, () => clock.Now, log.Add, delay), launcher, clock, log);
    }

    /// <summary>Starts an episode and waits out its first 30 s: the attempt this returns is the first.</summary>
    static async Task Begin(AppleDevicesStarter starter, Clock clock)
    {
        await starter.Update(true);
        clock.Add(30);
        await starter.Update(true);
    }

    [Fact]
    public async Task At_most_one_attempt_a_minute_and_three_an_episode()
    {
        var (starter, launcher, clock, log) = Make();
        // Nothing asked: nothing done, nothing logged.
        await starter.Update(false);
        Assert.Equal(0, launcher.Started);
        Assert.Empty(log);

        // The first attempt waits until the episode has lasted 30 s.
        await starter.Update(true);
        Assert.Equal(0, launcher.Started);
        Assert.Contains(log, l => l.Contains("first attempt in 30 s"));
        clock.Add(29);
        await starter.Update(true);
        Assert.Equal(0, launcher.Started);
        clock.Add(1);
        await starter.Update(true);
        Assert.Equal(1, launcher.Started);
        // Every few seconds the same word: no new attempt within the minute.
        for (var i = 0; i < 5; i++)
        {
            clock.Add(10);
            await starter.Update(true);
        }
        Assert.Equal(1, launcher.Started);
        clock.Add(10);
        await starter.Update(true);
        Assert.Equal(2, launcher.Started);
        clock.Add(60);
        await starter.Update(true);
        Assert.Equal(3, launcher.Started);
        // The third was the last of this episode, however long it lasts.
        for (var i = 0; i < 10; i++)
        {
            clock.Add(60);
            await starter.Update(true);
        }
        Assert.Equal(3, launcher.Started);
        Assert.Equal(3, starter.Attempts);
        Assert.Single(log, l => l.Contains("giving up"));
        Assert.Equal(3, log.Count(l => l.Contains(": done")));
        Assert.All(log, l => Assert.StartsWith("apple-devices: ", l));
        Assert.Contains(log, l => l.Contains("attempt 1/3"));
        Assert.Contains(log, l => l.Contains("attempt 3/3"));
    }

    [Fact]
    public async Task A_new_episode_starts_counting_again_but_keeps_the_minute()
    {
        var (starter, launcher, clock, log) = Make();
        await Begin(starter, clock);
        clock.Add(60);
        await starter.Update(true);
        Assert.Equal(2, launcher.Started);
        // Apple's service answers: the episode is over (and said so).
        clock.Add(5);
        await starter.Update(false);
        Assert.Equal(0, starter.Attempts);
        Assert.Contains(log, l => l.Contains("no longer needs starting (after 2 attempt(s))"));
        // It stops again, a little later: a new episode, with its own wait and its own three attempts, but still a
        // minute after the last attempt (40 s after it here: not yet).
        clock.Add(5);
        await starter.Update(true);
        clock.Add(30);
        await starter.Update(true);
        Assert.Equal(2, launcher.Started);
        clock.Add(19);
        await starter.Update(true);
        Assert.Equal(2, launcher.Started);
        clock.Add(1);
        await starter.Update(true);
        Assert.Equal(3, launcher.Started);
        clock.Add(60);
        await starter.Update(true);
        clock.Add(60);
        await starter.Update(true);
        clock.Add(60);
        await starter.Update(true);
        Assert.Equal(5, launcher.Started);
        // The end of an episode is said once.
        log.Clear();
        await starter.Update(false);
        await starter.Update(false);
        Assert.Single(log);
    }

    [Fact]
    public async Task Episodes_that_end_at_once_are_still_a_minute_apart_and_six_an_hour()
    {
        var (starter, launcher, clock, log) = Make();
        var started = new List<DateTimeOffset>();
        launcher.Next = () =>
        {
            started.Add(clock.Now);
            return Task.CompletedTask;
        };
        // usbmuxd keeps ending: every 35 s a new episode, each over 2 s after its attempt (or its chance of one).
        var end = clock.Now.AddHours(3);
        while (clock.Now < end)
        {
            await starter.Update(true);
            clock.Add(31);
            await starter.Update(true);
            clock.Add(2);
            await starter.Update(false);
            clock.Add(2);
        }
        Assert.All(started.Zip(started.Skip(1)), p => Assert.True(p.Second - p.First >= AppleDevicesStarter.Spacing, $"{p.First:T} {p.Second:T}"));
        foreach (var at in started)
        {
            Assert.True(started.Count(t => t >= at && t - at < AppleDevicesStarter.Window) <= AppleDevicesStarter.MaxPerWindow, $"{at:T}");
        }
        // Six in each of the three hours, give or take where the hours fall.
        Assert.InRange(started.Count, 16, 18);
        // The limit is said once each time it is reached (at most once an attempt), not on every episode.
        var said = log.Count(l => l.Contains("attempts in the last 60 minutes"));
        Assert.InRange(said, 1, started.Count);
    }

    [Fact]
    public async Task The_button_starts_usbmuxd_first_and_waits_for_it()
    {
        var waits = new List<TimeSpan>();
        var (starter, launcher, _, log) = Make(waits);
        // usbmuxd answers on the fourth look after it was started.
        var startedAt = 0;
        launcher.Next = () =>
        {
            startedAt = launcher.Calls.Count;
            return Task.CompletedTask;
        };
        launcher.Listening = () => startedAt > 0 && launcher.Calls.Count - startedAt > 3;
        await starter.OpenAsync();
        Assert.Equal("listening?", launcher.Calls[0]);
        Assert.Equal("start", launcher.Calls[1]);
        Assert.Equal("open", launcher.Calls[^1]);
        Assert.Equal(1, launcher.Calls.Count(c => c == "open"));
        Assert.Equal(1, launcher.Started);
        Assert.Equal(1, launcher.Opened);
        Assert.NotEmpty(waits);
        Assert.Contains(log, l => l.Contains("starting it first"));
        Assert.Contains(log, l => l.Contains("usbmuxd is listening"));
        Assert.Contains(log, l => l.Contains("opening Apple Devices: done"));
        // The button isn't paced and isn't counted: the automatic attempts go on as before.
        Assert.Equal(0, starter.Attempts);
    }

    [Fact]
    public async Task The_button_opens_at_once_when_usbmuxd_listens()
    {
        var (starter, launcher, _, _) = Make();
        launcher.Listening = () => true;
        await starter.OpenAsync();
        Assert.Equal(["listening?", "open"], launcher.Calls);
    }

    [Fact]
    public async Task The_button_opens_the_window_even_when_usbmuxd_never_comes()
    {
        var waits = new List<TimeSpan>();
        var (starter, launcher, _, log) = Make(waits);
        launcher.Next = () => throw new InvalidOperationException("Apple Devices from the Store is not installed for this user");
        await starter.OpenAsync();
        Assert.Equal(1, launcher.Started);
        Assert.Equal(1, launcher.Opened);
        Assert.Equal("open", launcher.Calls[^1]);
        Assert.Equal(AppleDevicesStarter.OpenWait, TimeSpan.FromTicks(waits.Sum(w => w.Ticks)));
        Assert.Contains(log, l => l.Contains("starting before opening: failed: InvalidOperationException"));
        Assert.Contains(log, l => l.Contains("still isn't listening after 10 s; opening anyway"));
        // A listener check that throws counts as not listening.
        var (other, otherLauncher, _, otherLog) = Make();
        otherLauncher.Listening = () => throw new System.Net.NetworkInformation.NetworkInformationException(5);
        await other.OpenAsync();
        Assert.Equal(1, otherLauncher.Opened);
        Assert.Contains(otherLog, l => l.Contains("couldn't tell whether usbmuxd listens"));
    }

    /// <summary>
    /// The button's start may hang (PowerShell up to a minute): the window opens after <see cref="AppleDevicesStarter.OpenWait"/>
    /// all the same, and no automatic attempt starts while that start is still going. An automatic attempt already
    /// going is waited on, not doubled (task-13 re-review N2).
    /// </summary>
    [Fact]
    public async Task The_button_does_not_wait_for_a_slow_start_or_start_twice()
    {
        var waits = new List<TimeSpan>();
        var (starter, launcher, clock, log) = Make(waits);
        var hang = new TaskCompletionSource();
        launcher.Next = () => launcher.Calls[^1] == "start" ? hang.Task : Task.CompletedTask;
        await starter.OpenAsync();
        Assert.Equal(1, launcher.Started);
        Assert.Equal(1, launcher.Opened);
        Assert.Equal(AppleDevicesStarter.OpenWait, TimeSpan.FromTicks(waits.Sum(w => w.Ticks)));
        Assert.DoesNotContain(log, l => l.Contains("starting before opening:"));
        // The automatic attempts wait for the button's start to end.
        clock.Add(120);
        await Begin(starter, clock);
        Assert.Equal(1, launcher.Started);
        hang.SetResult();
        await Task.Yield();
        Assert.Contains(log, l => l.Contains("starting before opening: done"));

        // An automatic attempt under way: the button waits for it rather than starting another.
        var (other, otherLauncher, otherClock, otherLog) = Make();
        var slow = new TaskCompletionSource();
        otherLauncher.Next = () => otherLauncher.Calls[^1] == "start" ? slow.Task : Task.CompletedTask;
        _ = Begin(other, otherClock);
        Assert.Equal(1, otherLauncher.Started);
        await other.OpenAsync();
        Assert.Equal(1, otherLauncher.Started);
        Assert.Equal(1, otherLauncher.Opened);
        Assert.Contains(otherLog, l => l.Contains("an attempt to start it is under way"));
        slow.SetResult();
    }

    [Fact]
    public void The_app_paces_by_a_clock_that_only_goes_forward()
    {
        var a = AppleDevicesStarter.MonotonicNow();
        var b = AppleDevicesStarter.MonotonicNow();
        Assert.True(b >= a);
        Assert.Equal(DateTimeOffset.UnixEpoch + TimeSpan.FromMilliseconds(Environment.TickCount64), b, TimeSpan.FromSeconds(5));
        var app = File.ReadAllText(Path.Combine(AppSources.AppDir, "App.xaml.cs"));
        Assert.Contains("new AppleDevicesStarter(new AppleDevicesLauncher(), AppleDevicesStarter.MonotonicNow, _log.Write)", app);
    }

    [Fact]
    public async Task Failures_are_logged_and_kept_in()
    {
        var (starter, launcher, clock, log) = Make();
        // Thrown at once, or a faulted task: the same.
        launcher.Next = () => throw new InvalidOperationException("class not registered");
        await Begin(starter, clock);
        Assert.Contains(log, l => l.Contains("attempt 1/3: failed: InvalidOperationException") && l.Contains("class not registered"));
        launcher.Next = () => Task.FromException(new System.Runtime.InteropServices.COMException("denied", unchecked((int)0x80070005)));
        clock.Add(60);
        await starter.Update(true);
        Assert.Contains(log, l => l.Contains("attempt 2/3: failed: COMException 0x80070005"));
        // A failed attempt still counts.
        Assert.Equal(2, starter.Attempts);
        // The button's failure, too.
        await starter.OpenAsync();
        Assert.Equal(1, launcher.Opened);
        Assert.Contains(log, l => l.Contains("opening Apple Devices: failed"));
        // And a log that throws stops nothing.
        var throwing = new AppleDevicesStarter(new FakeLauncher(), () => clock.Now, _ => throw new IOException("disk full"));
        await Begin(throwing, clock);
        await throwing.OpenAsync();
        Assert.Equal(1, throwing.Attempts);
    }

    [Fact]
    public async Task An_attempt_still_running_is_not_started_again()
    {
        var (starter, launcher, clock, log) = Make();
        var hung = new TaskCompletionSource();
        launcher.Next = () => hung.Task;
        await starter.Update(true);
        clock.Add(30);
        var first = starter.Update(true);
        clock.Add(120);
        await starter.Update(true);
        Assert.Equal(1, launcher.Started);
        hung.SetResult();
        await first;
        Assert.Contains(log, l => l.Contains("attempt 1/3: done"));
        launcher.Next = null;
        await starter.Update(true);
        Assert.Equal(2, launcher.Started);
    }

    [Fact]
    public async Task The_button_opens_apple_devices_whatever_the_pace()
    {
        var (starter, launcher, _, log) = Make();
        launcher.Listening = () => true;
        await starter.OpenAsync();
        await starter.OpenAsync();
        Assert.Equal(2, launcher.Opened);
        Assert.Equal(0, launcher.Started);
        Assert.Equal(2, log.Count(l => l.Contains("opening Apple Devices: done")));
    }

    [Fact]
    public void Error_text_becomes_one_short_line()
    {
        Assert.Equal("a | b | c", FileLog.OneLine("a\r\n  b \n\n c\r\n", 500));
        Assert.Equal("", FileLog.OneLine("\r\n", 500));
        var cut = FileLog.OneLine(new string('x', 600), 500);
        Assert.Equal(503, cut.Length);
        Assert.EndsWith("...", cut);
    }

    [Fact]
    public void The_log_appends_timestamped_lines_and_rolls_over()
    {
        var dir = Directory.CreateTempSubdirectory("cg-log-");
        try
        {
            var path = Path.Combine(dir.FullName, "sub", "app.log");
            var log = new FileLog(path, maxBytes: 200);
            log.Write("first");
            log.Write("second");
            var lines = File.ReadAllLines(path);
            Assert.Equal(2, lines.Length);
            Assert.Matches(@"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}[+-]\d\d:\d\d first$", lines[0]);
            Assert.EndsWith(" second", lines[1]);
            for (var i = 0; i < 10; i++)
            {
                log.Write("line " + i + new string('x', 20));
            }
            Assert.True(File.Exists(path + ".1"));
            Assert.True(new FileInfo(path).Length < 200 + 100);
            Assert.Contains("line 9", File.ReadAllText(path));
            // A path it can't write to is no failure.
            new FileLog(Path.Combine(dir.FullName, "sub", "app.log", "not-a-dir", "x.log")).Write("lost");
        }
        finally
        {
            dir.Delete(recursive: true);
        }
    }
}
