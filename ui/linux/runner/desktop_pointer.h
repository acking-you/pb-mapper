#ifndef RUNNER_DESKTOP_POINTER_H_
#define RUNNER_DESKTOP_POINTER_H_

#include <flutter_linux/flutter_linux.h>

// Registers the `pb_mapper/desktop_pointer` method channel (a remote
// computer's pointer bitmap as this computer's own cursor) on `view`'s
// engine, for as long as the view lives.
void desktop_pointer_register(FlView* view);

#endif  // RUNNER_DESKTOP_POINTER_H_
