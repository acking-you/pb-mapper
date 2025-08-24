# Senders/Receivers: An Introduction

**Senders/Receivers: An Introduction**

By Lucian Radu Teodorescu

*Overload, 32(184):11-16, December 2024*

***

C++26 will introduce a new concurrency feature called std::execution, or senders/receivers. Lucian Radu Teodorescu explains the idea and how to use these in detail.

In June 2024, at the WG21 plenary held in St. Louis, the P2300R10: `std::execution` paper \[[P2300R10](#_idTextAnchor003)], also known as senders/receivers, was formally adopted for inclusion in C++ 26. The content of the paper quickly found its way into the working draft for the C++ standard \[[WG21](#_idTextAnchor010)]. You can find more about the highlights of the St. Louis meeting in Herb Sutter’s trip report \[[Sutter24](#_idTextAnchor008)].

Senders/receivers represent one of the major additions to C++, as they provide an underlying model for expressing computations, adding support for concurrency, parallelism, and asynchrony. By using senders/receivers, one can write programs that heavily and efficiently exploit concurrency, all while maintaining thread safety (no deadlocks, race conditions, etc.). This is applicable not only to a few classes of concurrent problems but, at least in theory, to all types of concurrency problems. Senders/receivers provide a cost-free way of expressing computations that can run on different hardware with different constraints. They support creating computation chains that execute work on the CPU, GPU, and also enable non-blocking I/O.

Although the proposal has many advantages, there are still people who see the addition of this feature to the C++ standard at this point as a mistake. Some of the cited reasons are the complexity of the feature, compilation times, immaturity, and teachability. The last one caught my attention.

In this article, I plan to provide an introduction to senders/receivers as described in P2300 (and some related papers). The goal is not necessarily to showcase the many advantages of this model or delve into the details of complex topics. Rather, it is to offer a gentle introduction for those who have never read the paper or watched a talk on senders/receivers. We want the reader to understand the basic concepts of using senders/receivers without needing to grasp the intricate details of their implementation.

The hope is that, by the end of the article, the reader will be able to write some programs that use senders/receivers. The examples here are written as if the reader is coding with the feature already included in the standard library. Currently, no standard library provider ships senders/receivers; however, the reader can use the reference implementation of the feature \[[stdexec](#_idTextAnchor007)].

## Starting example

Listing 1 shows a simple example that prints *Hello, world!* using senders/receivers. Receivers don’t typically appear in the user code (they appear in the implementation of the algorithms that deal with senders), so we can also say that Listing 1 shows an example of using basic senders.

```
using stdexec = std::execution;
stdexec::sender auto computation
  = stdexec::just("Hello, world!")
  | stdexec::then([](std::string_view s) {
    std::print(s);
  });
std::this_thread::sync_wait(
  std::move(computation));
```

The example is equivalent (up to a point) to the code in Listing 2. We describe the action of printing *Hello, world!* to standard output; this description is stored in the variable computation. Then, we execute the action described by computation, producing the actual printing of the message. The action itself is composed of two parts: one that describes a string value and one that describes an action that takes the string and prints it out.

```
std::function<void()> computation = []{
  std::string_view s = "Hello, world!";
  std::print(s);
};
computation();
```
The code `just(X) | then(f)` describes work that is equivalent to `f(X)`. Adding another `then`, we have the work described by `just(X) | then(f) | then(g)` as equivalent to `g(f(X))`. If `f` and `g` don’t produce any values, then `just(X) | then(f) | then(g)` describes work equivalent to `f(X); g()`. **Senders are designed with composability in mind**; they allow expressing complex computations in terms of simpler ones.

The actual execution of the work described by `computation` occurs when `sync_wait` is invoked; if `sync_wait` were not present, no work would be executed.

Although simple, Listing 1 demonstrates a few important characteristics of working with senders:

* senders describe computations;
* senders are designed to compose well;
* senders are executed lazily; in our example, nothing happens until `sync_wait` is invoked.

In addition to these, there are two more important aspects of senders, both of which will be explored later in this article:

* senders can be used to describe concurrent/asynchronous work;
* senders enable structured concurrency.

Let’s look into the first point.

## Representing concurrency

The code in Listing 3 shows a simple example of executing code on a different thread. In the senders/receivers world, we don’t operate with threads; we operate with *schedulers*. Schedulers are handles to execution contexts; that is, schedulers provide access to one or more threads. Schedulers dictate *where* particular work needs to be executed.

```
stdexec::scheduler auto sch 
  = get_system_scheduler()
stdexec::sender auto computation
  = stdexec::schedule(sch)
  | stdexec::then([] {
    std::print("Hello, from a different thread");
  });
std::this_thread::sync_wait(
  std::move(computation));
```
In our example, we obtain the system scheduler. This is not part of the original P2300 \[[P2300R10](#_idTextAnchor003)] proposal, but it has been added as an extension through P2079: System execution context \[[P2079R5](#_idTextAnchor004)]; the idea of a system scheduler was deemed very important for inclusion in senders/receivers \[[P3109R0](#_idTextAnchor005)]. The system scheduler describes an execution context intended to be shared by all parts of the application or even across applications.

The call to `schedule(sch)` returns a sender. This sender represents work that starts on a thread belonging to the system execution context. It doesn’t send any value to the next sender but ensures that the work described by the next sender occurs on this thread.

The work described by `schedule(sch) | then(f)` is, to a point, equivalent to `std::thread([]{ f() })`, with the difference that the new thread is part of an execution context for which `sch` is a handle.

We use `schedule()` to start new work in an execution context, but sometimes we need to transfer execution from one context to another. For this, we can use the `continue_on()` algorithm. If we have a computation executed in one execution context and another computation that needs to be executed in a different context, we might use `continue_on()` to connect the two computations. For example, this chain describes work that executes `f` on the original thread and executes `g` on a (most likely) different thread represented by the scheduler `sch`:

```cpp
  just() | then(f) | continue_on(sch) | then(g)
```

With `schedule()` and `continues_on()` algorithms, one can implement any type of movement of work between threads. To make things easier to express in some cases, the senders/receivers proposal provides another algorithm: `starts_on()`. This can be used when we want to start a chain of work on a specific scheduler, but without specifying the scheduler in the work itself.

Listing 4 gives an example of `starts_on()` and of `continues_on()`. We have a sender that describes the work of reading data from a socket. In this description, we haven’t specified on which scheduler this needs to be executed. However, in the overall computation, the expression `starts_on(io_sched, std::move(read_data_snd))` ensures that the work is actually started in the context of the given I/O scheduler.

```cpp
stdexec::sender auto read_data_snd
  = stdexec::just(connection, buffer)
  | stdexec::then(read_data);
stdexec::sender auto process_all_snd
  = stdexec::starts_on(io_sched,
    std::move(read_data_snd))
  | stdexec::continues_on(work_sched)
  | stdexec::then(process_data)
  | stdexec::continues_on(io_sched)
  | stdexec::then(write_result);
std::this_thread::sync_wait(
  std::move(process_all_snd));
```

The example shows also a usage for `continues_on()`. The part that reads data from a socket (i.e., the work represented by `read_data_snd`) will be executed on the I/O scheduler. As we want the processing to happen on a ‘work scheduler’, we have to specify that the execution should switch threads. This is done by the `continues_on(work_sched)` expression. Similarly, after processing the data on the work scheduler, we want to go back to the I/O scheduler to write back the response. To do this, we call `continues_on()` again, passing the handle to the I/O scheduler.

One can see that moving between execution contexts is pretty easy, if we arrange the work so that such as it can be described by a chain of senders.

## Waiting for multiple senders

So far, we’ve seen examples in which different work items run on different threads, but all the examples assumed a sequenced execution of work items. We did not have an example in which two functions would run concurrently. Let’s correct that.

Listing 5 shows an example in which two functions `f` and `g` are run concurrently. To make this possible, we use the `when_all()` algorithm. This receives multiple senders and ensures that the results from all the senders are combined together before printing the results.

```cpp
stdexec::sender auto s1 = 
  stdexec::schedule(sch) | stdexec::then(f);
stdexec::sender auto s2 = 
  stdexec::schedule(sch) | stdexec::then(g);
stdexec::sender auto both_results = stdexec::when_all(s1, s2);
stdexec::sender auto print_results
  = std::move(both_results)
  | stdexec::then([](auto... args) {
    std::print("Results: {}, {}", args...);
  });
```

Both branches of work that go into the `when_all()` sender are started at the same time, but they are independent. Sometimes, we want to have some common processing, then execute two (or more) things concurrently, and then join the work chain together. This can be accomplished using the `split()` algorithm. Listing 6 (on the following page) shows an example of this. Here, when the work is started, function `p` is called first, and then `f` and `g` are called concurrently after `p` is finished.

```cpp
sender auto common = 
  schedule(sch) | then(p) | split();
sender auto s1 = common | then(f);
sender auto s2 = common | then(g);
sender auto both_results = when_all(s1, s2);
sender auto print_results
  = std::move(both_results)
  | then([](auto... args) {
    std::print("Results: {}, {}", args...);
  });
```

## Executing in bulk

The senders we’ve seen so far can only work on a single item at a given time. But what if we have many items that we need to work on? If one has N elements to process, one can use the `bulk()` algorithm to describe computations that process these elements.

Listing 7 presents an example of implementing the basic linear algebra *axpy* operation (from ‘a x plus y’) \[[Wikipedia-1](#_idTextAnchor011)]. For each index `i` in the range \[`0, x.size()`), we invoke the given lambda function.

```cpp
double a;
std::vector<double> x;
std::vector<double> y;
sender auto process_elements
  = just()
  | bulk(x.size(), [&](size_t i) {
    y[i] = a * x[i] + y[i]
  });
``` 

If the sender prior to applying `bulk()` produces a value, that value is passed to the functor given to `bulk()`; naturally, if the previous sender completes with multiple values, they are all passed to the functor. The same example can thus be written as in Listing 8.

```cpp
double some_value;
std::vector<double> x;
std::vector<double> y;
sender auto process_elements
  = just(some_value)
  | bulk(x.size(), [&](size_t i, double a) {
    y[i] = a * x[i] + y[i]
  });
```

## Shape of senders and structuredness

One important characteristic of senders that we haven’t discussed before is their shape. This allows senders to compose well, be extensible, and achieve structured concurrency.

Similar to a traditional function, the work represented by a sender has one entry point and one exit point, usually called *completion* (or *completion signal*). A function can either complete with a value or throw an exception – there are two ways a function can complete. A sender has a third type of completion indicating cancellation. In the world of senders/receivers, we name them as follows:

* `set_value(auto... values)` – used when the sender’s work successfully produces the output values;
* `set_error(auto err)` – used when the sender’s work completes with an error `err`;
* `set_stopped()` – used when the work represented by the sender is cancelled.

A traditional function can produce only one value. A sender, on the other hand, can produce multiple values; this is why the signature of `set_value()` allows multiple arguments. A traditional function can signal errors (that are different from return values) only through exceptions; a sender can represent work that can complete with an error of any type – `std::exception_ptr, std::error_code`, or any user-defined error type. When the work of a sender is cancelled, there is no value to produce, and thus, there is no argument to `set_stopped()`.

A regular function has one return type and can additionally produce exceptions. Thus, a function `T f(...)` can either complete with `T` or with an `std::exception_ptr`. There isn’t much variance possible with regular functions. The work of a sender, on the other hand, can complete with multiple types of values or multiple types of errors. More precisely, a sender can support any combination of completion signals. Some senders might complete with different sets of value types, while others might complete with different types of errors, and so on.

For example, we can have a sender that has the following completion signals:

* `set_value(int)`,
* `set_value(std::string)`,
* `set_value(int, std::string)`,
* `set_error(std::exception_ptr)`,
* `set_error(std::error_code)`,
* `set_stopped()`.

We can also have senders that complete with just a subset of these types of completion signals. For example, the sender returned by `just()` will only complete with `set_value()`, and the sender returned by `just(2, 3.14)` will only complete with `set_value(int, double)`. Similarly, the sender returned by `just_error("some error string"s)` will only complete with `set_error(std::string)`, and the sender returned by `just_stopped() `will only complete with `set_stopped()`.

These points suggest that senders are generalisations of functions, in the sense that they support multiple types of completion.

The choice of representing the completion signals as function calls is not accidental. This is how the work described by the senders actually calls the receivers. In P2300, a receiver is defined as “*a callback that supports more than one channel*” \[[P2300R10](#_idTextAnchor003)]. The end user does not need to be concerned with receivers; they serve merely as glue between senders. This is why, so far, we haven’t introduced them and have only discussed senders. We will continue to do so, as senders are the main focus.

There is another important aspect that needs to be addressed for senders. In a regular function, the completion happens on the same thread as the entry point. For the work represented by senders, this is not required. We can start on one thread and complete on another. For example, the `schedule(sch)` algorithm describes work that starts on a thread and moves control to a thread governed by `sch`. Another good example is the `continue_on()` algorithm.

From this perspective too, senders are a generalisation of functions. I can’t emphasise enough the importance of this. In non-concurrent code, structured programming taught us to work with functions. This means that with senders we can perform the same type of breakdown we were doing with functions. We can represent all parts of a program with senders, and we can even compose the entire program from senders. I’ve shown an example in the ‘Structured Concurrency’ *ACCU* talk \[[Teodorescu22](#_idTextAnchor009)].

As a consequence of senders describing work that behaves like functions, senders inherit structuredness properties. A sender contained within another sender must complete before its parent completes. We can have senders hide implementation details, thereby providing abstraction points. As mentioned above, we can decompose the program using senders.

In the end, all these structuredness properties make it easier to reason about the code. We can write good concurrent code without the fear of deadlocks and data races, simply by composing senders.

Senders can abstract work, so they can serve as an abstraction for any type of concurrent or asynchronous work. Here are a few examples:

* A sender can encapsulate a concurrent sort algorithm (which may run on the GPU or on the CPU) – an example of using senders to speed up programs.
* A sender can encapsulate the processing of an image; the processing can be done on a single thread, on multiple threads, or on GPUs – an example showing that concurrency concerns are hidden.
* A sender can encapsulate a `sleep` operation; the sender completes when the sleep period ends but doesn’t keep any thread busy – an example of asynchrony.
* A sender can encapsulate the wait for the results of a remote procedure call over the network, while not keeping the local threads busy – another example of asynchrony.

## Sender algorithms in the standard

The P2300 proposal \[[P2300R10](#_idTextAnchor003)], which was merged into the working draft for C++ 26, contains a set of algorithms that operate on senders. Because of their structuredness properties, senders compose well, so we should be able to build larger senders from smaller ones.

The C++ 26 standard will include several sender algorithms to be used as primitives for building more complex senders. These are grouped into three categories:

* **Sender factories**: They produce senders without requiring any other senders. Algorithms in the standard: `schedule()`, `just()`, `just_error()`, `just_stopped()`, `read_env()`.
* **Sender adaptors**: Given one or more senders, they return senders based on the provided senders. Algorithms in the standard: `starts_on()`, `continues_on()`, `schedule_from()`, `on()`, `then()`, `upon_error()`, `upon_stopped()`, `let_value()`, `let_error()`, `let_stopped()`, `bulk()`, `split()`, `when_all()`, `into_variant()`, `stopped_as_optional()`, `stopped_as_error()`.
* **Sender consumers**: They consume senders but don’t produce any senders. Algorithms in the standard: `sync_wait()`, `sync_wait_with_variant()`.

All the sender factories and adaptors are defined in the `std::execution` namespace. The sender consumer algorithms are defined in the `std::this_thread` namespace.

We will briefly go through each of these algorithms.

### Sender factories

We’ve already seen examples of the `just()` algorithm. This is used to create a sender that completes with the given values. We’ve also seen the `just_error()` algorithm, which creates a sender that completes with the given error. We’ve mentioned the `just_stopped()` algorithm as well; this algorithm produces a sender that completes with a `set_stopped() `signal.

The `read_env()` algorithm is more advanced. Given a *tag*, it tries to retrieve the property for that tag from the execution environment. That is, if we have a child sender inside a parent sender, the child sender can use `read_env()` to obtain various properties from the parent sender.

### Sender adaptors

Before describing the actual sender adaptor algorithms, it’s worth highlighting an important aspect of the syntax for most of these adaptors: there are two forms for the algorithm. We have a canonical form and a *pipeable* form. The best way to explain this is with an example, and the `then()` algorithm is likely the best choice for illustrating this.

The canonical form of `then()` is: `then(sndr, ftor)`. When this is used, it returns a sender that, when `sndr` completes, applies `ftor` to its produced values and completes with the transformed values (function composition).

The piped form of `then()` is `then(ftor)`. This form should only be used in a piped context. An expression of the form `sndr | then(ftor)` is equivalent to calling `then(sndr, ftor)`. Usually, the piped form is easier to write, so many people prefer it.

Technically, `then(ftor)` is a *sender adaptor closure*, not a sender. The then sender also includes the previous sender, i.e., what comes before the pipe operator. However, colloquially we often refer to it as a sender, for simplicity.

Similar to the `then()` algorithm, we have `upon_error()` and `upon_stopped()`. They function in the same way as `then()`, but are applied to the error or stop completion channels, respectively. `upon_error()` applies the given functor to the incoming error and completes with the result of the function application. `upon_stopped()` calls the given functor and completes with `set_stopped()`.

We’ve already seen examples of `starts_on()` and `continues_on()`. The `on()` algorithm is a combination of these two: it executes work on a given scheduler (similar to `starts_on()`) but returns to the original scheduler upon completion (resembling `continues_on()`).

The `schedule_from()` algorithm is a foundational operation for `continues_on()`. It’s not meant to be called directly by users but can be useful for specialising some of the transitions between schedulers.

We’ve also briefly described above the algorithms `bulk()` (used to execute the same function multiple times for a range of indices), `split()` (used to ensure that the same sender can be contained in the same chain of computation without executing the same work twice), and `when_all()` (used to combine the results of multiple senders).

The `let_*()` family of algorithms is important, yet they are often misunderstood. The `let_value()` algorithm is similar to the `then()` algorithm, but the given functor is expected to return a sender. This is the monadic bind operation for senders, i.e., a fundamental building block for senders. It is similar to the `optional<T>::and_then()` function (part of the so-called `std::optional` monadic operations).

Instead of this abstract explanation, let’s illustrate with an example. Suppose we have a pipeline for performing image transformations (e.g., automatically enhancing an image). We want to abstract this pipeline, so we encapsulate the pipeline building into a function `enhance_image_sndr()` that takes an image as an argument and returns a sender that knows how to enhance the image. Using a pseudo-syntax, we would say that the type of `enhance_image_sndr()` is `Image -> Sender<Image>`. Now, we want to put this pipeline inside another pipeline that first loads the image, enhances it, and then writes it to the destination storage (disk, network, etc.). We cannot inject this function into our flow with `then()`; that would produce a `Sender<Sender<Image>>` instead of `Sender<Image>`. For that, we have `let_value()`. Listing 9 shows how the code may look.

```cpp
// Returns a sender that produces 'Image' values
auto enhance_image_sndr(Image img) {...}
Image load();
void save(Image);
sender auto complete_pipeline
  = just()
  | then(load)
  | let_value([](Image img) {
    return enhance_image_sndr(img); })
  | then(save);
```
                                                                                                                                                                                                                                                                      Similar to `let_value()`, the `let_error()` algorithm performs the same job, but applies the given functor to the error produced by the previous sender. Additionally, `let_stopped()` applies the given functor when a stopped signal is received.

The remaining three sender adaptor algorithms (`into_variant()`, `stopped_as_optional()`, and `stopped_as_error()`) are designed to make it easier to work with different types of completion signals.

The first one, `into_variant()`, adapts a sender that might have multiple value completion signatures into a sender with a single completion signature consisting of an `std::variant` of `std::tuples`. It doesn’t change any error or stopped completions. For example, if `snd` can complete with `set_value(std::string)` or `set_value(int, double)`, then `into_variant(snd)` is a sender that can complete with:

```
  set_value(std::variant<std::tuple<std::string>,
            std::tuple<int, double>>)
```

The `stopped_as_optional()` algorithm removes the need for a stopped completion by transforming it into an empty optional value. Additionally, it transforms the value completion from a type `T` to an `std::optional<T>`. Thus, if `snd` is a sender that completes with either a value of `int` or a stopped signal, then `stopped_as_optional(snd)` will complete only with a value of `std::optional<int>`.

The `stopped_as_error()` algorithm behaves similarly but transforms a stopped completion signal into an error completion. Thus, if `snd` is a sender that completes with either a value of `int` or a stopped signal, then `stopped_as_error(snd, err)` will complete only with a value of type `int` or the error `err`.

### Sender consumers

The main sender consumer algorithm defined by the proposal is `sync_wait()`. We’ve seen this in our examples above. This algorithm takes one sender as input and performs the following actions:

* submits the work described by the given sender;

* blocks the current thread until the sender’s work is finished;

* returns the result of the sender’s work in the appropriate form to the caller:

  * returns an optional tuple of values – those that the given sender completes with – if the sender completes with `set_value()`;
  * throws the received error if the sender completes with `set_error()`;
  * returns an empty optional if the given sender completes with a stopped signal.

For a sender `snd` that completes with `set_value(int, double)`, the resulting type of `sync_wait(snd)` is:

```
  std::optional<std::tuple<int, double>>
```

If `snd` completes with a value of type `int`, then `sync_wait(snd)` returns `std::optional<std::tuple<int>>` (not dropping the tuple part). If the given sender doesn’t send a stopped completion signal, the return type will still contain the optional part, even if there will always be a value present.

An interesting restriction of this algorithm is that the given sender cannot complete with more than one `set_value()` signal. This is because the return type, as defined, cannot accommodate multiple value completion types.

If we have a sender that completes with multiple types of value signals, we can use the `sync_wait_with_variant()` algorithm. This is similar to `sync_wait()`, but its return type is an `std::optional` of an `std::variant` of `std::tuples`. For example, for a sender `snd` that can complete with `set_value(std::string)` and `set_value(int, double)`, `sync_wait_with_variant(snd)` returns:

```cpp
  std::optional<std::variant<std::tuple
    <std::string>, std::tuple<int, double>>>
```

It may sound a bit complex, but it’s straightforward with a bit of practice. After all, this is the most logical conclusion when considering the possible completion types for a sender.

## Beyond P2300

The above section may have made it seem like P2300 proposes numerous algorithms to fully cover the needs of concurrency and asynchrony, but this is far from the truth. It simply lays the foundation for building basic senders. In fact, there is a paper, P3109R0: ‘A plan for `std::execution` for C++26’ \[[P3109R0](#_idTextAnchor005)], adopted by the standard committee, which details work we aim to include in the C++ standard and which is not part of P2300. This paper mentions three important facilities that would have a significant impact on end-users:

* system execution context;
* async scope;
* coroutine task type.

The current senders/receiver proposal, as merged into the standard, doesn’t define any scheduler, so users may need to write their own schedulers to describe concurrent work. Previous versions of senders/receivers defined a thread pool scheduler, but this was later removed due to numerous issues. The system execution context proposal \[[P2079R5](#_idTextAnchor004)] introduces a scheduler type that makes use of the system’s execution context. On Windows, it should use the Windows Thread Pool \[[Microsoft](#_idTextAnchor002)] to schedule work, and on macOS, it should use Grand Central Dispatch \[[GCD](#_idTextAnchor001)]. Aiming to reduce CPU oversubscription \[[Wikipedia-2](#_idTextAnchor012)], the system scheduler is a good default for spawning CPU-intensive work. We’ve already seen an example of this in Listing 3.

Until recently, the P2300 proposal, which introduced senders/receivers, included two algorithms called `start_detached()` and `ensure_started()` that would submit the work for a sender eagerly, without a way to join the work. These two algorithms would allow the user to implement unstructured concurrency, as the work spawned by these two algorithms outlives the work that spawned them. (Currently, the only way to submit work is through `sync_wait()`, which is fully structured.) While unstructured concurrency can lead to various issues, it is often useful to have a way to spawn large work from a narrow scope.

The async scope proposal \[[P3149R6](#_idTextAnchor006)] allows the user to have a weakly-structured way of launching work. It defines an async scope in which we can dynamically launch work that outlives the scope from which it was spawned. The key point is that all work spawned within this async scope must be joined before the scope is destroyed. This means that we allow some unstructuredness, but we contain it within a defined scope.

In addition to enabling some unstructuredness, async scope is also useful for launching a dynamic number of work items and then joining that work within a fully structured context.

The third major feature is a coroutine task type. This would essentially mean writing an `std::execution::task<T>` coroutine that can seamlessly interoperate with senders. Using this, one can `co_await` a sender or consider such a coroutine to be a sender. Thus, this task type can freely interoperate with a sender. This would allow users to write coroutines to handle concurrency and asynchrony instead of using compositions of sender algorithms to build them. While there may be some performance penalties involved with using such a task type, users may prefer it for certain types of programs, as the code is more readable.

Other senders/receivers features that would be highly desirable in C++ but were not part of P3109 include:

* C++ parallel algorithms (synchronous) (P2500)
* C++ asynchronous parallel algorithms (P3300)
* I/O and time-based schedulers
* networking on top of senders/receivers

## Conclusions

Senders/Receivers is a new C++ feature that provides a model for expressing computations, supporting concurrency, parallelism, and asynchrony. It allows for structured concurrency, making it easier to reason about concurrent code and avoid common pitfalls. Senders/Receivers has already been voted into C++ and is expected to land in C++ 26.

This article provides an introduction to the subject of senders/receivers so that people can start using it as soon as it’s available. Although this feature is used for concurrency, we presented it organically, starting with building computations and touching on the concurrency aspects without needing to explain too much about threading and execution contexts. This is one of the beauties of the model: it abstracts away concurrency concerns without compromising performance or safety.

We’ve spent a fair amount of time explaining the idea behind senders so that readers can easily grasp the key aspects of the proposal and start writing programs using senders/receivers.

The article didn’t go into detail on how to use senders/receivers to implement complex problems. Some of these examples can be found on the Internet, in various talks and examples. And perhaps that’s a good topic for a follow-up article.

## References

[]()\[GCD] Apple, Grand Central Dispatch, 2016, <https://swiftlang.github.io/swift-corelibs-libdispatch/>.

[]()\[Microsoft] Microsoft, ‘Thread Pools’, 2021, <https://learn.microsoft.com/en-us/windows/win32/procthread/thread-pools>.

[]()\[P2300R10] Michał Dominiak, Georgy Evtushenko, Lewis Baker, Lucian Radu Teodorescu, Lee Howes, Kirk Shoop, Michael Garland, Eric Niebler, Bryce Adelstein Lelbach, P2300R10: ‘`std::execution`’, 2024, <https://wg21.link/P2300R10>.

[]()\[P2079R5] Lucian Radu Teodorescu, Ruslan Arutyunyan, Lee Howes, Michael Voss, P2079R5: ‘System execution context’, 2024, <https://wg21.link/P2079R5>.

[]()\[P3109R0] Lewis Baker, Eric Niebler, Kirk Shoop, Lucian Radu Teodorescu, P3109R0: ‘A plan for `std::execution` for C++26’, 2024, <https://wg21.link/P3109R0>.

[]()\[P3149R6] Ian Petersen, Jessica Wong, Ján Ondrušek, Kirk Shoop, Lee Howes, Lucian Radu Teodorescu, P3149R6: ‘async\_scope – Creating scopes for non-sequential concurrency’, <https://wg21.link/P3149R6>.

[]()\[stdexec] NVIDIA, ‘Senders – A Standard Model for Asynchronous Execution in C++’, <https://github.com/NVIDIA/stdexec>.

[]()\[Sutter24] Herb Sutter, Trip report: Summer ISO C++ standards meeting (St Louis, MO, USA), 2024, <https://herbsutter.com/2024/07/02/trip-report-summer-iso-c-standards-meeting-st-louis-mo-usa/>.

[]()\[Teodorescu22] Lucian Radu Teodorescu, Structured Concurrency, ACCU Conference, 2022, <https://www.youtube.com/watch?v=Xq2IMOPjPs0>.

[]()\[WG21] WG21, ‘Execution control library’ in *Working Draft Programming Languages – C++* <https://eel.is/c++draft/#exec>.

[]()\[Wikipedia-1] Wikipedia, Basic Linear Algebra Subprograms, <https://en.wikipedia.org/wiki/Basic_Linear_Algebra_Subprograms#Level_1>.

[]()\[Wikipedia-2] Wikipedia, Resource contention, <https://en.wikipedia.org/wiki/Resource_contention>.

Lucian Radu Teodorescu has a PhD in programming languages and is a Staff Engineer at Garmin. He likes challenges; and understanding the essence of things (if there is one) constitutes the biggest challenge of all.

##### Advertisement

[![](https://ads.accu.org/www/delivery/avw.php?zoneid=2\&cb=879648978587659\&n=ac1df087)](https://ads.accu.org/www/delivery/ck.php?n=ac1df087\&cb=879648978587659)

\


[![](/img/accu/join.png)](/menu-overviews/membership/)

#### FAQ:

About Us

[About ACCU ](/faq/about)[What is ACCU? An Editorial](/faq/what-is)

\
[Advertise with ACCU](/faq/advertise)\
[Conferences](/faq/conference)\
[Committee Members](/faq/short-committee)\
[Constitution](/faq/constitution)\
[Values](/faq/values)\
[Study Groups](/faq/study-groups-faq)\
[Mailing Lists](/faq/mailing-lists-faq)\
[Privacy Policy](/faq/privacy-policy)\
[Cookie Policy](/faq/cookie-policy)

***

#### Members Only:

[Study Groups](/members/study-groups)\
[Mailing Lists](/members/mailing-lists)\
[Review a Book](/members/review-a-book)\


ACCU Committee

[Committee Members ](/members/long-committee)[Posts and Roles ](/members/posts-and-roles)[Attending Meetings ](/members/attending-meetings)[Minutes ](/members/minutes)[Archive](/members/archive)

\
[Annual General Meetings](/members/agm)\
[Complaints Procedure](/members/complaints)\


Member's Account

[Subscription Details ](/actions/display-subs)[Update Member Details ](/actions/display-details)[Update Password ](/actions/change-password)[Update Email Address ](/actions/change-email)[Other Member Updates](/members/member-updates)

\
[Log Out](/loginout/log-out)

***

#### Contact:

[General Information](mailto:info@accu.org?subject=\[ACCU])\
[Membership](mailto:accumembership@accu.org?subject=\[ACCU])\
[Local Groups](mailto:localgroups@accu.org?subject=\[ACCU])\
[Advertising](mailto:ads@accu.org?subject=\[ACCU])\
[Web Master](mailto:webmaster@accu.org?subject=\[ACCU])\
[Web Editor](mailto:webeditor@accu.org?subject=\[ACCU])\
[Conference](mailto:conference@accu.org?subject=\[ACCU])

***

#### Links:

[World of Code](https://blogs.accu.org)\
[Essential Books](https://github.com/accu-org/essential-books/wiki)\
[Bluesky](https://bsky.app/profile/accuorg.bsky.social)\
[Mastodon](https://mastodon.social/@ACCU)\
[Facebook](https://www.facebook.com/accuorg)\
[LinkedIn](https://www.linkedin.com/company/accu/)\
[GitHub](https://www.github.com/accu-org)\
[flickr](https://www.flickr.com/groups/accu-org)\
[YouTube](https://www.youtube.com/channel/UCJhay24LTpO1s4bIZxuIqKw)\
[RSS Feed](https://accu.org/index.xml)

***
