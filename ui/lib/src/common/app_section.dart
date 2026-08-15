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
/// Logs used to be a tab here. They now live in both workspaces, where the
/// question they answer is actually asked, and a copy under ops would only be
/// a second door onto the same view.
enum OpsTab {
  /// Whether the server is up, and what it is holding.
  status,

  /// What is registered on it. This shared the status page, which made one
  /// screen answer two questions and left the list of services — the longer
  /// of the two, and the one you scroll — squeezed into half a window.
  services,

  config,
}
