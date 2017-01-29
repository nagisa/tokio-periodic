extern crate futures;
extern crate tokio_core;
extern crate tokio_periodic;

fn main() {
    let mut core = tokio_core::reactor::Core::new().unwrap();
    let handle = core.handle();
    let timer = tokio_periodic::PeriodicTimer::new(&handle).unwrap();
    timer.reset(::std::time::Duration::new(1, 0));
    let digits = futures::stream::unfold(1, |v| {
        Some(futures::future::ok((v, v + 1)))
    });
    let mut timer_stream = futures::Stream::zip(timer, digits);
    while let Ok((Some(item), stream)) = core.run(futures::Stream::into_future(timer_stream)) {
        println!("{}", item.1);
        timer_stream = stream;
    }
}
