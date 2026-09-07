//! Подробный вывод (`verbose`), оформленный по образцу `dbg!`.
//!
//! Все макросы печатают в stderr (как и `dbg!`) и молчат, пока
//! `cfg.verbose == false`. Первым аргументом всегда идёт что-либо,
//! из чего можно получить флаг — см. [`Verbosity`].

/// Источник флага `verbose`. Реализовано для `Config` и `bool`, поэтому
/// в макросы можно передавать и `&cfg`, и просто `true`/`false`.
pub trait Verbosity {
    fn verbose(&self) -> bool;
}

impl Verbosity for bool {
    fn verbose(&self) -> bool {
        *self
    }
}

impl Verbosity for crate::config::Config {
    fn verbose(&self) -> bool {
        self.verbose
    }
}

impl<T: Verbosity + ?Sized> Verbosity for &T {
    fn verbose(&self) -> bool {
        (**self).verbose()
    }
}

/// Аналог `dbg!`, работающий только при включённом `verbose`.
///
/// Печатает `[verbose] file:line] expr = value` и возвращает значение,
/// как это делает `dbg!`.
///
/// ```ignore
/// let res = vdbg!(&cfg, db.add_message(msg).await);
/// ```
#[macro_export]
macro_rules! vdbg {
    ($v:expr $(,)?) => {{
        if $crate::verbose::Verbosity::verbose(&$v) {
            eprintln!("[verbose] {}:{}]", file!(), line!());
        }
    }};
    ($v:expr, $val:expr $(,)?) => {
        match $val {
            tmp => {
                if $crate::verbose::Verbosity::verbose(&$v) {
                    eprintln!(
                        "[verbose] {}:{}] {} = {:#?}",
                        file!(),
                        line!(),
                        stringify!($val),
                        &tmp
                    );
                }
                tmp
            }
        }
    };
    ($v:expr, $($val:expr),+ $(,)?) => {
        ($($crate::vdbg!($v, $val)),+,)
    };
}

/// Строковое сообщение в том же формате, что и [`vdbg!`], но без выражения.
///
/// ```ignore
/// vlog!(&cfg, "connecting to database: {}", cfg.database_url);
/// ```
#[macro_export]
macro_rules! vlog {
    ($v:expr, $($arg:tt)*) => {{
        if $crate::verbose::Verbosity::verbose(&$v) {
            eprintln!("[verbose] {}:{}] {}", file!(), line!(), format!($($arg)*));
        }
    }};
}
