/// Where the user is in the app.
///
/// The app has three levels rather than one flat list of pages. Registering a
/// service and connecting to one are separate jobs, usually on different
/// machines, so a workspace shows only the job at hand. Status, config and logs
/// are operations work: reachable from anywhere, but never mixed into a task.
enum AppSection {
  /// First run: a guided wizard instead of a page of choices.
  setup,

  /// Pick a role, or head into ops.
  home,

  /// Publishing local services.
  register,

  /// Subscribing to remote services.
  connect,

  /// Status and configuration.
  ops;

  bool get isWorkspace => this == register || this == connect;
}

/// The tabs inside the ops zone.
///
/// Logs used to be a third tab here. They now live in both workspaces, where
/// the question they answer is actually asked, and a copy under ops would only
/// be a second door onto the same view.
enum OpsTab { status, config }
