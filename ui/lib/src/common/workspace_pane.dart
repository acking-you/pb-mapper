/// Which half of a workspace is on screen.
///
/// Registering and connecting each have a form and a list of what already
/// exists. Stacking them made the list something you had to scroll to find, so
/// the sidebar offers them as two destinations and each one gets the window.
enum WorkspacePane {
  /// The form that creates something new.
  form,

  /// What has been created already.
  list,
}
