use dioxus::prelude::*;

const ECHO_CSS: Asset = asset!("/assets/styling/echo.css");

/// Echo component that demonstrates fullstack server functions.
#[component]
pub fn Echo() -> Element {
    // use_signal is a hook. Hooks in dioxus must be run in a consistent order every time the component is rendered.
    // That means they can't be run inside other hooks, async blocks, if statements, or loops.
    //
    // use_signal is a hook that creates a state for the component. It takes a closure that returns the initial value of the state.
    // The state is automatically tracked and will rerun any other hooks or components that read it whenever it changes.
    let mut response = use_signal(|| String::new());

    rsx! {
        document::Link { rel: "stylesheet", href: ECHO_CSS }

        div {
            id: "echo",
            h4 { "ServerFn Echo" }
            input {
                placeholder: "Type here to echo...",
                // `oninput` is an event handler that will run when the input changes. It can return either nothing or a future
                // that will be run when the event runs.
                oninput:  move |event| async move {
                    // When we call the echo_server function from the client, it will fire a request to the server and return
                    // the response. It handles serialization and deserialization of the request and response for us.
                    let data = echo_server(event.value()).await.unwrap();

                    // After we have the data from the server, we can set the state of the signal to the new value.
                    // Since we read the `response` signal later in this component, the component will rerun.
                    response.set(data);
                },
            }

            // Signals can be called like a function to clone the current value of the signal
            if !response().is_empty() {
                p {
                    "Server echoed: "
                    // Since we read the signal inside this component, the component "subscribes" to the signal. Whenever
                    // the signal changes, the component will rerun.
                    i { "{response}" }
                }
            }
        }
    }
}

// Server functions let us define public APIs on the server that can be called like a normal async function from the client.
// Each server function needs to be annotated with the `#[server]` attribute, accept and return serializable types, and return
// a `Result` with the error type [`ServerFnError`].
//
// When the server function is called from the client, it will just serialize the arguments, call the API, and deserialize the
// response.
#[server]
async fn echo_server(input: String) -> Result<String, ServerFnError> {
    // The body of server function like this comment are only included on the server. If you have any server-only logic like
    // database queries, you can put it here. Any imports for the server function should either be imported inside the function
    // or imported under a `#[cfg(feature = "server")]` block.
    Ok(input)
}
