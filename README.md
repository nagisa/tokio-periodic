[![Travis CI][tcii]][tci] [![Appveyor CI][acii]][aci]

[tcii]: https://travis-ci.org/nagisa/tokio-periodic.svg?branch=master
[tci]: https://travis-ci.org/nagisa/tokio-periodic
[acii]: https://ci.appveyor.com/api/projects/status/7nmr4q4wd3rxkc27/branch/master?svg=true
[aci]: https://ci.appveyor.com/project/nagisa/tokio-periodic

Asynchronous, cross-platform periodic timers

These timers (unlike in tokio-timer, which implements this in user-space) are implemented using the
OS facilities in as zero-overhead manner as possible.

# Usage

In `Cargo.toml`:

```
[dependencies]
tokio-periodic = "0.1"
```

Then, in your crate root add:

```
extern crate tokio_periodic;
```
