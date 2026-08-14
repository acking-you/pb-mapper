/// Which part of a workspace is on screen.
///
/// Registering and connecting each have a form and a list of what already
/// exists. Stacking them made the list something you had to scroll to find, so
/// the sidebar offers them as separate destinations and each one gets the
/// window.
enum WorkspacePane {
  /// The form that creates something new.
  form,

  /// What has been created already.
  list,

  /// The log stream.
  ///
  /// Logs are how you find out why a registration did not come up, which is a
  /// question you have while standing in the workspace. Keeping them only under
  /// ops meant leaving the job to go read about it, so they are a destination
  /// here too — the same view ops shows, not a second one.
  logs,
}
