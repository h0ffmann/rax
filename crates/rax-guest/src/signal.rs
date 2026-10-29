//! Mapping between the API's signals and the host's signal numbers.

use crate::proto::Signal;

const TABLE: &[(Signal, libc::c_int)] = &[
    (Signal::Hup, libc::SIGHUP),
    (Signal::Int, libc::SIGINT),
    (Signal::Quit, libc::SIGQUIT),
    (Signal::Ill, libc::SIGILL),
    (Signal::Trap, libc::SIGTRAP),
    (Signal::Abrt, libc::SIGABRT),
    (Signal::Bus, libc::SIGBUS),
    (Signal::Fpe, libc::SIGFPE),
    (Signal::Kill, libc::SIGKILL),
    (Signal::Usr1, libc::SIGUSR1),
    (Signal::Segv, libc::SIGSEGV),
    (Signal::Usr2, libc::SIGUSR2),
    (Signal::Pipe, libc::SIGPIPE),
    (Signal::Alrm, libc::SIGALRM),
    (Signal::Term, libc::SIGTERM),
    (Signal::Chld, libc::SIGCHLD),
    (Signal::Cont, libc::SIGCONT),
    (Signal::Stop, libc::SIGSTOP),
    (Signal::Tstp, libc::SIGTSTP),
    (Signal::Ttin, libc::SIGTTIN),
    (Signal::Ttou, libc::SIGTTOU),
    (Signal::Urg, libc::SIGURG),
    (Signal::Xcpu, libc::SIGXCPU),
    (Signal::Xfsz, libc::SIGXFSZ),
    (Signal::Vtalrm, libc::SIGVTALRM),
    (Signal::Prof, libc::SIGPROF),
    (Signal::Winch, libc::SIGWINCH),
    (Signal::Io, libc::SIGIO),
    (Signal::Sys, libc::SIGSYS),
];

/// The host signal for an API signal, by name, so the mapping holds on
/// platforms whose numbers differ from Linux. `None` for unspecified or
/// unknown values.
pub fn to_syscall_signal(sig: Signal) -> Option<libc::c_int> {
    TABLE.iter().find(|(s, _)| *s == sig).map(|(_, n)| *n)
}

/// The API signal for a host signal, or `Unspecified` when it has none.
pub fn from_syscall_signal(sig: libc::c_int) -> Signal {
    TABLE
        .iter()
        .find(|(_, n)| *n == sig)
        .map_or(Signal::Unspecified, |(s, _)| *s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for (api, sys) in TABLE {
            assert_eq!(to_syscall_signal(*api), Some(*sys));
            assert_eq!(from_syscall_signal(*sys), *api);
        }
        assert_eq!(to_syscall_signal(Signal::Unspecified), None);
        assert_eq!(from_syscall_signal(0), Signal::Unspecified);
    }
}
