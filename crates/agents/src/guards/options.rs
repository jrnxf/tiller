//! Option parsing for the programs the Bash guard unwraps, read the way GNU
//! `getopt_long` reads them in `+` mode: options end at the first operand
//! or at `--`, short options bundle (`-fo out`), a value is attached
//! (`-oout`, `--output=out`) or the next word, and a long option may be
//! shortened to any prefix that names one option.
//!
//! An option table is a string of entries split by whitespace. An entry
//! lists the spellings of one option split by `|`: one letter is a short
//! option, a longer word a long one. A spelling ending in `=` takes a value,
//! attached or as the next word; one ending in `?` takes a value only when
//! attached. The option's name is its first long spelling, or its first
//! spelling when it has no long one.

use super::bash::Arg;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Takes {
    Nothing,
    Value,
    Attached,
}

struct Entry {
    name: &'static str,
    shorts: Vec<(char, Takes)>,
    longs: Vec<(&'static str, Takes)>,
}

fn table(spec: &'static str) -> Vec<Entry> {
    spec.split_whitespace()
        .map(|entry| {
            let mut shorts = Vec::new();
            let mut longs = Vec::new();
            for spelling in entry.split('|') {
                let (bare, takes) = if let Some(bare) = spelling.strip_suffix('=') {
                    (bare, Takes::Value)
                } else if let Some(bare) = spelling.strip_suffix('?') {
                    (bare, Takes::Attached)
                } else {
                    (spelling, Takes::Nothing)
                };
                let mut chars = bare.chars();
                match (chars.next(), chars.next()) {
                    (Some(letter), None) => shorts.push((letter, takes)),
                    _ => longs.push((bare, takes)),
                }
            }
            let first = entry.split('|').next().unwrap_or(entry);
            let name = longs
                .first()
                .map(|(long, _)| *long)
                .unwrap_or_else(|| first.trim_end_matches(['=', '?']));
            Entry {
                name,
                shorts,
                longs,
            }
        })
        .collect()
}

/// The long option `key` names: an exact spelling, or a prefix of the
/// spellings of exactly one option.
fn long_option<'t>(table: &'t [Entry], key: &str) -> Option<(&'t Entry, Takes)> {
    if key.is_empty() {
        return None;
    }
    for entry in table {
        if let Some((_, takes)) = entry.longs.iter().find(|(long, _)| *long == key) {
            return Some((entry, *takes));
        }
    }
    let mut matches = table.iter().filter_map(|entry| {
        entry
            .longs
            .iter()
            .find(|(long, _)| long.starts_with(key))
            .map(|(_, takes)| (entry, *takes))
    });
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

/// One option the program reads.
pub(super) struct Found {
    /// The option's name in its table.
    pub name: &'static str,
    /// The word that holds it, as written.
    pub spelling: String,
    pub value: Option<Arg>,
    /// The index just after the words this option used.
    pub end: usize,
}

pub(super) enum Parse {
    Read {
        found: Vec<Found>,
        /// The index of the first operand, after `--` when there is one.
        next: usize,
        /// The options ended at `--`.
        ended: bool,
    },
    /// A word in an option position that is not known before it runs, at
    /// `at`. It may be an operand that ends the options, or an option;
    /// `option` is true when it can only be an option (it starts with `-`).
    Unfixed {
        found: Vec<Found>,
        at: usize,
        option: bool,
    },
    /// An option the program does not take, or an option the guard cannot
    /// read.
    Unsure,
}

impl Parse {
    /// The parse for a caller that does not read the forms of a word it
    /// cannot see: such a word is [`Parse::Unsure`].
    pub(super) fn sure(self) -> Parse {
        match self {
            Parse::Unfixed { .. } => Parse::Unsure,
            parse => parse,
        }
    }
}

/// Text in an unfixed word that the shell may change.
fn dynamic(text: &str) -> bool {
    text.contains(['$', '`', '*', '?', '[', '{', '~'])
}

/// Reads the options at the start of `words` by `spec`. With `numeric`,
/// a word such as `-5`, `--5` or `-+5` is an option named `number` too
/// (`nice -5`).
pub(super) fn parse(spec: &'static str, numeric: bool, words: &[Arg]) -> Parse {
    let table = table(spec);
    let mut found = Vec::new();
    let mut i = 0;
    while let Some(word) = words.get(i) {
        let text = word.text.as_str();
        if !word.fixed {
            // `--name=$VALUE`: a known option with a value the guard cannot
            // read. Any other unfixed word may be anything.
            let readable = text
                .strip_prefix("--")
                .and_then(|long| long.split_once('='))
                .is_some_and(|(key, _)| !key.is_empty() && !dynamic(key));
            if !readable {
                return Parse::Unfixed {
                    found,
                    at: i,
                    option: text.starts_with('-') && text != "-",
                };
            }
        }
        if word.is("--") {
            return Parse::Read {
                found,
                next: i + 1,
                ended: true,
            };
        }
        if !text.starts_with('-') || text == "-" {
            break;
        }
        i += 1;
        let after = &text[1..];
        if numeric
            && after
                .strip_prefix(['-', '+'])
                .unwrap_or(after)
                .starts_with(|c: char| c.is_ascii_digit())
        {
            found.push(Found {
                name: "number",
                spelling: text.to_owned(),
                value: None,
                end: i,
            });
            continue;
        }
        if let Some(long) = text.strip_prefix("--") {
            let (key, attached) = match long.split_once('=') {
                Some((key, value)) => (key, Some(value)),
                None => (long, None),
            };
            let Some((entry, takes)) = long_option(&table, key) else {
                return Parse::Unsure;
            };
            let value = match (takes, attached) {
                (Takes::Nothing, Some(_)) => return Parse::Unsure,
                (_, Some(value)) => Some(Arg {
                    text: value.to_owned(),
                    fixed: word.fixed,
                }),
                (Takes::Nothing | Takes::Attached, None) => None,
                (Takes::Value, None) => match words.get(i) {
                    Some(next) => {
                        i += 1;
                        Some(next.clone())
                    }
                    // The program fails on the missing value.
                    None => {
                        return Parse::Read {
                            found,
                            next: i,
                            ended: false,
                        };
                    }
                },
            };
            found.push(Found {
                name: entry.name,
                spelling: text.to_owned(),
                value,
                end: i,
            });
            continue;
        }
        let cluster = &text[1..];
        for (at, letter) in cluster.char_indices() {
            let Some((entry, takes)) = table.iter().find_map(|entry| {
                entry
                    .shorts
                    .iter()
                    .find(|(short, _)| *short == letter)
                    .map(|(_, takes)| (entry, *takes))
            }) else {
                return Parse::Unsure;
            };
            let rest = &cluster[at + letter.len_utf8()..];
            let value = match takes {
                Takes::Nothing => {
                    found.push(Found {
                        name: entry.name,
                        spelling: text.to_owned(),
                        value: None,
                        end: i,
                    });
                    continue;
                }
                Takes::Attached => (!rest.is_empty()).then(|| Arg::fixed(rest)),
                Takes::Value if !rest.is_empty() => Some(Arg::fixed(rest)),
                Takes::Value => match words.get(i) {
                    Some(next) => {
                        i += 1;
                        Some(next.clone())
                    }
                    None => {
                        return Parse::Read {
                            found,
                            next: i,
                            ended: false,
                        };
                    }
                },
            };
            found.push(Found {
                name: entry.name,
                spelling: text.to_owned(),
                value,
                end: i,
            });
            break;
        }
    }
    Parse::Read {
        found,
        next: i,
        ended: false,
    }
}

/// GNU coreutils `env`, plus the BSD `env` options `-P`, `-L` and `-U`.
pub(super) const ENV: &str = "a=|argv0= i|ignore-environment 0|null u=|unset= C=|chdir= \
    S=|split-string= block-signal? default-signal? ignore-signal? list-signal-handling \
    v|debug help version P= L= U=";

/// The `exec` builtin of bash and zsh.
pub(super) const EXEC: &str = "c l a=";

/// `sudo` 1.9.
pub(super) const SUDO: &str = "A|askpass a=|auth-type= B|bell b|background C=|close-from= \
    c=|login-class= D=|chdir= E|preserve-env? e|edit g=|group= H|set-home h?|help host= \
    i|login K|remove-timestamp k|reset-timestamp l|list N|no-update n|non-interactive \
    P|preserve-groups p=|prompt= R=|chroot= r=|role= S|stdin s|shell T=|command-timeout= \
    t=|type= U=|other-user= u=|user= V|version v|validate";

/// OpenBSD `doas`.
pub(super) const DOAS: &str = "C= L n s u=";

/// GNU findutils `xargs`, plus the BSD `xargs` options `-J`, `-R` and `-S`.
pub(super) const XARGS: &str = "0|null a=|arg-file= d=|delimiter= E= e?|eof? I= i?|replace? \
    J= L= l?|max-lines? n=|max-args= o|open-tty P=|max-procs= p|interactive \
    process-slot-var= R= r|no-run-if-empty S= s=|max-chars= show-limits t|verbose x|exit \
    help version";

/// `fish`.
pub(super) const FISH: &str = "c=|command= C=|init-command= d=|debug= o=|debug-output= \
    D=|debug-stack-frames= f=|features= h|help i|interactive l|login N|no-config \
    n|no-execute p=|profile= profile-startup= P|private print-rusage-self \
    print-debug-categories v|version";

/// A program whose remaining words, after its options and operands, are a
/// command line it runs.
pub(super) struct Runner {
    pub options: &'static str,
    /// `-N` is an option too (`nice -5`).
    pub numeric: bool,
    /// Operands before the command: `timeout DURATION`, `chroot DIR`.
    pub operands: usize,
    /// The operand is a number, and the command starts there when the word
    /// is not one (`chrt [PRIORITY] COMMAND`).
    pub numeric_operand: bool,
    /// Options after which the words are process ids, not a command.
    pub no_command: &'static [&'static str],
    /// Options that clear the environment.
    pub clears: &'static [&'static str],
    /// Options whose value is `NAME=VALUE` to set or `NAME` to unset.
    pub env: &'static [&'static str],
    /// Options whose value is a file the program writes.
    pub writes: &'static [&'static str],
    /// With no command, the program starts a shell, which reads its script
    /// on standard input.
    pub shell: bool,
}

const fn runs(options: &'static str) -> Runner {
    Runner {
        options,
        numeric: false,
        operands: 0,
        numeric_operand: false,
        no_command: &[],
        clears: &[],
        env: &[],
        writes: &[],
        shell: false,
    }
}

/// A strace output value that starts with `|` or `!` is a command strace
/// pipes its trace to.
const STRACE: Runner = Runner {
    env: &["env"],
    writes: &["output"],
    ..runs(
        "a=|columns= A|output-append-mode b=|detach-on= c|summary-only C|summary d|debug \
         D|daemonize? e= E=|env= f|follow-forks F h|help i|instruction-pointer \
         I=|interruptible= k|stack-trace? n|syscall-number o=|output= \
         O=|summary-syscall-overhead= p=|attach= P=|trace-path= q|quiet? \
         r|relative-timestamps? s=|string-limit= S=|summary-sort-by= t|absolute-timestamps? \
         T|syscall-times? u=|user= U=|summary-columns= v|no-abbrev V|version \
         w|summary-wall-clock x|strings-in-hex? X=|const-print-style= y|decode-fds? \
         Y|decode-pids? z|successful-only Z|failed-only trace= abbrev= verbose= raw= read= \
         write= fault= inject= status= signal= seccomp-bpf output-separately trace-fds= \
         tips? kill-on-exit syscall-limit= pidns-translation secontext? always-show-pid \
         timestamps? stack-traces? namespace? kvm= argv0=",
    )
};

const LTRACE: Runner = Runner {
    writes: &["output"],
    ..runs(
        "a=|align= A= b|no-signals c C|demangle d|debug? D= e= f F=|config= h|help i \
         l=|library= L n=|indent= o=|output= p= r s= S t T u= V|version w=|where= x=",
    )
};

const NICE: Runner = Runner {
    numeric: true,
    ..runs("n=|adjustment= help version")
};

const IONICE: Runner = Runner {
    no_command: &["pid", "pgid", "uid"],
    ..runs("c=|class= n=|classdata= p=|pid= P=|pgid= u=|uid= t|ignore h|help V|version")
};

const TIMEOUT: Runner = Runner {
    operands: 1,
    ..runs("f|foreground k=|kill-after= p|preserve-status s=|signal= v|verbose help version")
};

const NOHUP: Runner = runs("help version");

const CHRT: Runner = Runner {
    operands: 1,
    numeric_operand: true,
    no_command: &["pid", "max"],
    ..runs(
        "a|all-tasks b|batch d|deadline e|ext f|fifo i|idle m|max o|other p|pid r|rr \
         R|reset-on-fork v|verbose T=|sched-runtime= P=|sched-period= D=|sched-deadline= \
         h|help V|version",
    )
};

const TASKSET: Runner = Runner {
    operands: 1,
    no_command: &["pid"],
    ..runs("a|all-tasks c|cpu-list p|pid h|help V|version")
};

const SETSID: Runner = runs("c|ctty f|fork w|wait h|help V|version");

const STDBUF: Runner = runs("i=|input= o=|output= e=|error= help version");

const UNSHARE: Runner = Runner {
    shell: true,
    ..runs(
        "m|mount? u|uts? i|ipc? n|net? p|pid? U|user? C|cgroup? T|time? f|fork kill-child? \
         mount-proc? mount-binfmt? map-user= map-users= map-group= map-groups= map-auto \
         map-subids r|map-root-user c|map-current-user propagation= setgroups= keep-caps \
         S=|setuid= G=|setgid= R=|root= w=|wd= l=|load-interp= monotonic= boottime= h|help \
         V|version",
    )
};

const NSENTER: Runner = Runner {
    shell: true,
    ..runs(
        "a|all t=|target= m?|mount? u?|uts? i?|ipc? n?|net? N=|net-socket= p?|pid? U?|user? \
         user-parent C?|cgroup? T?|time? G=|setgid= S=|setuid= keep-caps preserve-credentials \
         r?|root? w?|wd? W?|wdns? e|env F|no-fork Z|follow-context c|join-cgroup h|help \
         V|version",
    )
};

const SETPRIV: Runner = Runner {
    clears: &["reset-env"],
    ..runs(
        "d|dump nnp|no-new-privs inh-caps= ambient-caps= bounding-set= ruid= euid= rgid= \
         egid= reuid= regid= clear-groups keep-groups init-groups groups= securebits= \
         pdeathsig= ptracer= selinux-label= apparmor-profile= landlock-access= landlock-rule= \
         seccomp-filter= reset-env list-caps h|help V|version",
    )
};

/// `runuser` and util-linux `su`: read with their own rules in the Bash
/// guard.
pub(super) const RUNUSER: Runner = Runner {
    clears: &["login"],
    ..runs(
        "c=|command= f|fast g=|group= G=|supp-group= l|login m|p|preserve-environment P|pty \
         s=|shell= session-command= T|no-pty u=|user= w=|whitelist-environment= h|help \
         V|version",
    )
};

const CHROOT: Runner = Runner {
    operands: 1,
    shell: true,
    ..runs("groups= userspec= skip-chdir help version")
};

const FAKEROOT: Runner = Runner {
    shell: true,
    ..runs("l=|lib= faked= i= s= u|unknown-is-real b=|fd-base= h|help v|version")
};

/// `flock FILE COMMAND` or `flock FILE -c COMMAND`: the Bash guard reads the
/// `-c` form after the operand, and the operand as a file flock creates.
pub(super) const FLOCK: Runner = Runner {
    operands: 1,
    ..runs(
        "s|shared x|e|exclusive u|unlock n|nb|nonblock w=|wait=|timeout= \
         E=|conflict-exit-code= o|close F|no-fork verbose fcntl h|help V|version",
    )
};

/// GNU `time`, plus the BSD `time` options `-l` and `-h`.
const TIME: Runner = Runner {
    writes: &["output"],
    ..runs("f=|format= o=|output= p|portability a|append v|verbose q|quiet V|version help l h")
};

const CAFFEINATE: Runner = runs("d i m s u t= w=");

const SYSTEMD_INHIBIT: Runner =
    runs("what= who= why= mode= list no-ask-password no-pager no-legend h|help version");

/// The programs that run their remaining words as a command line.
pub(super) fn runner(program: &str) -> Option<&'static Runner> {
    Some(match program {
        "strace" => &STRACE,
        "ltrace" => &LTRACE,
        "nice" => &NICE,
        "ionice" => &IONICE,
        "timeout" => &TIMEOUT,
        "nohup" => &NOHUP,
        "chrt" => &CHRT,
        "taskset" => &TASKSET,
        "setsid" => &SETSID,
        "stdbuf" => &STDBUF,
        "unshare" => &UNSHARE,
        "nsenter" => &NSENTER,
        "setpriv" => &SETPRIV,
        "runuser" => &RUNUSER,
        "chroot" => &CHROOT,
        "fakeroot" => &FAKEROOT,
        "flock" => &FLOCK,
        "time" => &TIME,
        "caffeinate" => &CAFFEINATE,
        "systemd-inhibit" => &SYSTEMD_INHIBIT,
        _ => return None,
    })
}

/// Splits an `env -S` string into words the way GNU `env` does: blanks
/// separate words, `'...'` and `"..."` quote, a backslash escapes, `\_` is
/// a blank, `\c` and a `#` that starts a word end the string, and
/// `${NAME}` is a variable, so a word that holds one is not fixed. `None`
/// when `env` rejects the string.
pub(super) fn split_string(text: &str) -> Option<Vec<Arg>> {
    let chars: Vec<char> = text.chars().collect();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut fixed = true;
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        i += 1;
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (None, '\'' | '"') => {
                quote = Some(c);
                started = true;
            }
            (None, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c') => {
                if started {
                    words.push(Arg {
                        text: std::mem::take(&mut word),
                        fixed,
                    });
                    fixed = true;
                    started = false;
                }
            }
            (None, '#') if !started => break,
            (Some('\''), '\\') => match chars.get(i) {
                Some(&next @ ('\\' | '\'')) => {
                    word.push(next);
                    i += 1;
                }
                _ => word.push('\\'),
            },
            (Some('\''), _) => word.push(c),
            (_, '\\') => {
                let next = *chars.get(i)?;
                i += 1;
                match next {
                    'c' => break,
                    '_' if quote.is_none() => {
                        if started {
                            words.push(Arg {
                                text: std::mem::take(&mut word),
                                fixed,
                            });
                            fixed = true;
                            started = false;
                        }
                        continue;
                    }
                    '_' => word.push(' '),
                    'f' => word.push('\x0c'),
                    'n' => word.push('\n'),
                    'r' => word.push('\r'),
                    't' => word.push('\t'),
                    'v' => word.push('\x0b'),
                    '\\' | '\'' | '"' | '$' | '#' => word.push(next),
                    _ => return None,
                }
                started = true;
            }
            (_, '$') => {
                if chars.get(i) != Some(&'{') {
                    return None;
                }
                let close = chars[i..].iter().position(|c| *c == '}')?;
                word.push_str(&chars[i - 1..i + close + 1].iter().collect::<String>());
                i += close + 1;
                fixed = false;
                started = true;
            }
            _ => {
                word.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if started {
        words.push(Arg { text: word, fixed });
    }
    Some(words)
}
