using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;

namespace CellGlide.Tests;

public class ViewModelTests
{
    static (FakeCore Core, EventHub Hub) Fake(ReplyResult? autoReply = null)
    {
        var core = new FakeCore(autoReply);
        return (core, new EventHub(core, new InlineDispatcher()));
    }

    [Fact]
    public void Shell_takes_state_and_service_gone()
    {
        var (core, hub) = Fake();
        var shell = new ShellViewModel(hub);
        core.Raise(new StateEvent(Samples.Overview()));
        Assert.Equal(Samples.Overview().Title, shell.Overview!.Title);
        // ServiceGone carries the chrome (Task 9/11); the shell takes it as it comes.
        core.Raise(new ServiceGoneEvent("gone", false, Samples.Chrome()));
        Assert.Equal("gone", shell.ServiceDown!.Text);
        Assert.Equal(Samples.Chrome().Tray, shell.ServiceDown.Chrome.Tray);
        Assert.Null(shell.Overview);
        core.Raise(new StateEvent(Samples.Overview()));
        Assert.Null(shell.ServiceDown);
    }

    [Fact]
    public async Task Toggle_sends_the_toggle_command()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        await new ShellViewModel(hub).ToggleCommand.ExecuteAsync(null);
        Assert.IsType<ToggleCommand>(Assert.Single(core.Sent));
    }

    [Fact]
    public async Task Subscribe_sends_subscribe()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        await new ShellViewModel(hub).SubscribeAsync();
        Assert.IsType<SubscribeCommand>(Assert.Single(core.Sent));
    }

    [Fact]
    public async Task Failed_command_sets_error_text()
    {
        var (_, hub) = Fake(autoReply: new ReplyErr("服务已停止"));
        var checks = new ChecksViewModel(hub);
        await checks.RunCommand.ExecuteAsync(null);
        Assert.Equal("服务已停止", checks.Error);
        Assert.Null(checks.Rows);
    }

    [Fact]
    public async Task Checks_run_the_doctor_and_take_its_rows()
    {
        var reply = Assert.IsType<ReplyEvent>(Samples.Event("reply_checks.json"));
        var (core, hub) = Fake(autoReply: reply.Result);
        var checks = new ChecksViewModel(hub);
        await checks.RunCommand.ExecuteAsync(null);
        Assert.IsType<DoctorRequest>(Assert.IsType<RequestCommand>(Assert.Single(core.Sent)).Request);
        Assert.Same(((ChecksValue)((ReplyOk)reply.Result).Value).Rows, checks.Rows);
        Assert.Null(checks.Error);
    }

    [Fact]
    public async Task About_takes_the_reply()
    {
        var reply = Assert.IsType<ReplyEvent>(Samples.Event("reply_about.json"));
        var (core, hub) = Fake(autoReply: reply.Result);
        var about = new AboutViewModel(hub);
        Assert.Null(about.About);
        await about.LoadCommand.ExecuteAsync(null);
        Assert.IsType<GetAboutCommand>(Assert.Single(core.Sent));
        Assert.Equal("https://t.me/cellglide", about.About!.TelegramUrl);
        Assert.Contains("CommunityToolkit.Mvvm", about.About.Licenses);
        Assert.Null(about.Error);
    }

    [Fact]
    public async Task About_failure_leaves_it_unset_with_the_sentence()
    {
        var (_, hub) = Fake(autoReply: new ReplyErr("服务已停止"));
        var about = new AboutViewModel(hub);
        await about.LoadCommand.ExecuteAsync(null);
        Assert.Null(about.About);
        Assert.Equal("服务已停止", about.Error);
    }

    [Fact]
    public async Task Speed_start_takes_include_wifi_from_the_prefs()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        var settings = new SettingsViewModel(hub);
        var speed = new SpeedViewModel(hub, settings);
        core.Raise(new PrefsEvent(new Prefs(false, false, false)));
        await speed.StartCommand.ExecuteAsync(true);
        Assert.Equal(new SpeedStartCommand(false, true, null), Assert.Single(core.Sent));

        core.Raise(new PrefsEvent(new Prefs(false, true, false)));
        await speed.StartCommand.ExecuteAsync(false);
        Assert.Equal(new SpeedStartCommand(true, false, null), core.Sent[1]);
    }

    [Fact]
    public async Task Speed_start_reads_the_prefs_when_none_came()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new PrefsValue(new Prefs(false, true, false))));
        var speed = new SpeedViewModel(hub, new SettingsViewModel(hub));
        await speed.StartCommand.ExecuteAsync(false);
        Assert.IsType<GetPrefsCommand>(core.Sent[0]);
        Assert.Equal(new SpeedStartCommand(true, false, null), core.Sent[1]);
    }

    [Fact]
    public async Task Speed_cancel_and_clear_send_their_commands()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        var speed = new SpeedViewModel(hub, new SettingsViewModel(hub));
        await speed.CancelCommand.ExecuteAsync(null);
        await speed.ClearHistoryCommand.ExecuteAsync(null);
        Assert.IsType<SpeedCancelCommand>(core.Sent[0]);
        Assert.IsType<SpeedHistoryClearCommand>(core.Sent[1]);
        var view = Assert.IsType<SpeedProgressEvent>(Samples.Event("speed_progress_data_warning.json")).View;
        core.Raise(new SpeedProgressEvent(view));
        Assert.Same(view, speed.View);
    }

    [Fact]
    public async Task Leak_start_cancel_and_view()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        var leak = new LeakViewModel(hub);
        await leak.StartCommand.ExecuteAsync(null);
        await leak.CancelCommand.ExecuteAsync(null);
        Assert.IsType<LeakStartCommand>(core.Sent[0]);
        Assert.IsType<LeakCancelCommand>(core.Sent[1]);
        var view = Assert.IsType<LeakProgressEvent>(Samples.Event("leak_progress_result.json")).View;
        core.Raise(new LeakProgressEvent(view));
        Assert.Same(view, leak.View);
        Assert.NotNull(leak.Checks);
    }

    [Fact]
    public async Task History_loads_the_day_asked_for()
    {
        var reply = Assert.IsType<ReplyEvent>(Samples.Event("reply_history.json"));
        var (core, hub) = Fake(autoReply: reply.Result);
        var history = new HistoryViewModel(hub);
        await history.LoadCommand.ExecuteAsync("2026-10-27");
        await history.LoadCommand.ExecuteAsync(null);
        Assert.Equal(new HistoryRequest("2026-10-27"), Assert.IsType<RequestCommand>(core.Sent[0]).Request);
        Assert.Equal(new HistoryRequest(null), Assert.IsType<RequestCommand>(core.Sent[1]).Request);
        Assert.Same(((HistoryValue)((ReplyOk)reply.Result).Value).View, history.View);
    }

    [Fact]
    public async Task Diagnostics_prepare_send_and_save()
    {
        var core = new ScriptedCore();
        var hub = new EventHub(core, new InlineDispatcher());
        var diagnostics = new DiagnosticsViewModel(hub);
        Assert.False(diagnostics.SendCommand.CanExecute(null));

        core.Next = new ReplyOk(new DiagnosticsValue("{}", "cellglide.json"));
        await diagnostics.PrepareCommand.ExecuteAsync(new DiagnosticsInput(DiagRange.Week, "a@b.co", "hi"));
        Assert.Equal(new DiagnosticsPrepareCommand(DiagRange.Week, "a@b.co", "hi"), core.Sent[0]);
        Assert.Equal("{}", diagnostics.Json);
        Assert.Equal("cellglide.json", diagnostics.FileName);
        Assert.True(diagnostics.SendCommand.CanExecute(null));

        core.Next = new ReplyOk(new SentValue("R-123"));
        await diagnostics.SendCommand.ExecuteAsync(null);
        Assert.Equal(new DiagnosticsSendCommand("{}"), core.Sent[1]);
        Assert.Equal("R-123", diagnostics.Code);

        core.Next = new ReplyOk(new SavedValue(@"C:\Users\me\Downloads\cellglide.json"));
        await diagnostics.SaveCommand.ExecuteAsync(null);
        Assert.Equal(new DiagnosticsSaveCommand("{}", null, false), core.Sent[2]);
        Assert.Equal(@"C:\Users\me\Downloads\cellglide.json", diagnostics.SavedPath);

        // A file the user chose and confirmed replacing in the save dialog.
        core.Next = new ReplyOk(new SavedValue(@"D:\report.json"));
        await diagnostics.SaveCommand.ExecuteAsync(new DiagnosticsSaveTarget(@"D:\report.json", true));
        Assert.Equal(new DiagnosticsSaveCommand("{}", @"D:\report.json", true), core.Sent[3]);
        Assert.Equal(@"D:\report.json", diagnostics.SavedPath);

        core.Next = new ReplyErr("no network");
        await diagnostics.SendCommand.ExecuteAsync(null);
        Assert.Equal("no network", diagnostics.Error);
    }

    [Fact]
    public async Task Settings_send_one_field_at_a_time()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        var settings = new SettingsViewModel(hub);
        await settings.SetAutoConnectCommand.ExecuteAsync(true);
        await settings.SetPhoneCommand.ExecuteAsync(new SerialPhoneChoice("00008140"));
        await settings.SetStrictDnsCommand.ExecuteAsync(false);
        await settings.SetPrefsCommand.ExecuteAsync(new Prefs(true, false, true));
        Assert.Equal(new SetSettingsRequest(true, null, null), Assert.IsType<RequestCommand>(core.Sent[0]).Request);
        Assert.Equal(new SetSettingsRequest(null, new SerialPhoneChoice("00008140"), null), Assert.IsType<RequestCommand>(core.Sent[1]).Request);
        Assert.Equal(new SetSettingsRequest(null, null, false), Assert.IsType<RequestCommand>(core.Sent[2]).Request);
        Assert.Equal(new SetPrefsCommand(new Prefs(true, false, true)), core.Sent[3]);
        Assert.NotNull(settings.Checks);
    }

    [Fact]
    public async Task Settings_take_prefs_from_the_reply_and_the_event()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new PrefsValue(new Prefs(true, true, true))));
        var settings = new SettingsViewModel(hub);
        Assert.Equal(new Prefs(true, true, true), await settings.LoadAsync());
        core.Raise(new PrefsEvent(new Prefs(false, false, false)));
        Assert.Equal(new Prefs(false, false, false), settings.Prefs);
    }

    [Fact]
    public async Task Speed_start_sends_the_caps_when_set()
    {
        var (core, hub) = Fake(autoReply: new ReplyOk(new DoneValue()));
        var settings = new SettingsViewModel(hub);
        var speed = new SpeedViewModel(hub, settings);
        Assert.Null(speed.Caps);
        core.Raise(new PrefsEvent(new Prefs(true, true, false)));
        speed.Caps = new SpeedCaps(20_000_000, 10_000_000, 10);
        await speed.StartCommand.ExecuteAsync(true);
        Assert.Equal(new SpeedStartCommand(true, true, new SpeedCaps(20_000_000, 10_000_000, 10)), Assert.Single(core.Sent));
    }

    [Fact]
    public async Task A_command_cancelled_by_dispose_leaves_no_error()
    {
        var (_, hub) = Fake();
        var checks = new ChecksViewModel(hub);
        var run = checks.RunCommand.ExecuteAsync(null);
        hub.Dispose();
        await run.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);
        Assert.Null(checks.Error);
        Assert.Null(checks.Rows);
    }

    [Fact]
    public async Task A_reply_that_cannot_reach_the_ui_is_an_internal_error()
    {
        var core = new FakeCore(new ReplyOk(new DoneValue()));
        var hub = new EventHub(core, new ThrowingDispatcher());
        var shell = new ShellViewModel(hub);
        await shell.ToggleCommand.ExecuteAsync(null);
        // FakeCore's Tr gives back the key.
        Assert.Equal("reason.internal", shell.Error);
    }

    sealed class ThrowingDispatcher : IUiDispatcher
    {
        public void Post(Action action) => throw new InvalidOperationException("ui thread gone");
    }

    /// <summary>Answers each command, inline, with whatever <see cref="Next"/> is.</summary>
    sealed class ScriptedCore : ICore
    {
        ulong _next;

        public ReplyResult Next { get; set; } = new ReplyOk(new DoneValue());

        public List<Command> Sent { get; } = [];

        public event Action<CoreEvent>? EventReceived;

        public ulong Send(Command command)
        {
            Sent.Add(command);
            var id = ++_next;
            EventReceived?.Invoke(new ReplyEvent(id, Next));
            return id;
        }

        public string Tr(string key, IReadOnlyDictionary<string, string>? args = null) => key;

        public void Dispose()
        {
        }
    }
}
