//! Schedule evaluation for the jobs subsystem - the "when" half of a scheduled command.
//!
//! Deliberately dependency-free and hand-scrolled (ADR-007): two spellings cover what a local
//! gateway actually needs, and both are pure functions that test without a clock.
//! - "everySec": a plain interval, anchored on the job's last run (or gateway boot for a job
//!   that has never run). Sub-minute precision, no calendar meaning.
//! - "cron": the classic 5-field expression (minute hour day-of-month month day-of-week),
//!   evaluated in LOCAL wall-clock time - "30 3 * * *" means 03:30 where the machine lives,
//!   which is what every cron a user has ever written meant. chrono::Local supplies the offset;
//!   chrono is already in the graph through sqlx (features unify additively), so this costs
//!   nothing the shipping binary did not already carry.
//!
//! The matcher implements vixie-cron's day-field rule on purpose: when BOTH day-of-month and
//! day-of-week are restricted (anything other than a bare "*"), a minute matches if EITHER
//! field matches - the OR, not the AND. Getting this wrong is the classic cron-port bug.

use chrono::{Datelike, TimeDelta, Timelike};

/// One field's worth of a cron expression: the matched values as a bitmask over the field's
/// legal range, plus whether the field was a bare "*" (which the day-of-month/day-of-week OR
/// rule treats as "unrestricted").
#[derive(Clone, Copy)]
struct Field<const LO: u8, const HI: u8> {
    mask: u64,
    star: bool,
}

impl<const LO: u8, const HI: u8> Field<LO, HI> {
    fn parse(text: &str) -> Result<Self, String> {
        let mut mask: u64 = 0;
        for item in text.split(',') {
            // "a/step" and "a-b/step" scale the range; no "/step" means step 1.
            let (range, step) = match item.split_once('/') {
                Some((r, s)) => {
                    let step: u32 = s
                        .parse()
                        .map_err(|_| format!("bad step {s:?} in cron field {text:?}"))?;
                    if step == 0 {
                        return Err(format!("step 0 in cron field {text:?}"));
                    }
                    (r, step)
                }
                None => (item, 1u32),
            };
            // vixie-cron: "n/step" means "n-max/step" - the open-ended shorthand.
            let (lo, hi) = if range == "*" {
                (LO, HI)
            } else if let Some((a, b)) = range.split_once('-') {
                (val(a, text)?, val(b, text)?)
            } else {
                let v = val(range, text)?;
                if step == 1 {
                    (v, v)
                } else {
                    (v, HI)
                }
            };
            if lo < LO || hi > HI {
                return Err(format!(
                    "value {lo}-{hi} outside {}..={} in cron field {text:?}",
                    LO, HI
                ));
            }
            if lo > hi {
                return Err(format!("descending range {lo}-{hi} in cron field {text:?}"));
            }
            let mut v = lo as u64;
            while v <= hi as u64 {
                mask |= 1 << v;
                v += step as u64;
            }
        }
        if mask == 0 {
            return Err(format!("empty cron field {text:?}"));
        }
        Ok(Field {
            mask,
            star: text == "*",
        })
    }

    fn matches(&self, v: u8) -> bool {
        self.mask & (1 << v) != 0
    }
}

fn val(raw: &str, whole: &str) -> Result<u8, String> {
    raw.parse::<u8>()
        .map_err(|_| format!("bad value {raw:?} in cron field {whole:?}"))
}

/// A parsed 5-field cron expression. Constructed through [CronExpr::parse].
#[derive(Clone)]
pub struct CronExpr {
    minutes: Field<0, 59>,
    hours: Field<0, 23>,
    doms: Field<1, 31>,
    months: Field<1, 12>,
    dows: Field<0, 7>,
}

/// The local-time facts of one minute a schedule is tested against.
pub struct MinuteFields {
    pub minute: u8,
    pub hour: u8,
    pub day: u8,
    pub month: u8,
    /// 0 = Sunday .. 6 = Saturday, cron's numbering.
    pub dow: u8,
}

pub fn fields_of(t: &chrono::NaiveDateTime) -> MinuteFields {
    MinuteFields {
        minute: t.minute() as u8,
        hour: t.hour() as u8,
        day: t.day() as u8,
        month: t.month() as u8,
        dow: t.weekday().num_days_from_sunday() as u8,
    }
}

impl CronExpr {
    pub fn parse(expr: &str) -> Result<Self, String> {
        let parts: Vec<&str> = expr.split_whitespace().collect();
        if parts.len() != 5 {
            return Err(format!(
                "cron expression must have 5 fields (minute hour day month weekday), got {}: {expr:?}",
                parts.len()
            ));
        }
        let dows: Field<0, 7> = Field::parse(parts[4])?;
        let mut expr = CronExpr {
            minutes: Field::parse(parts[0])?,
            hours: Field::parse(parts[1])?,
            doms: Field::parse(parts[2])?,
            months: Field::parse(parts[3])?,
            dows,
        };
        // cron counts 7 as Sunday too; fold it onto 0 so "0 0 * * 7" is Sunday, as in vixie-cron.
        expr.dows.mask |= (expr.dows.mask >> 7) & 1;
        Ok(expr)
    }

    /// Whether this expression fires in the minute described by "f".
    pub fn matches(&self, f: &MinuteFields) -> bool {
        // vixie-cron's day rule: both day fields restricted -> OR; either unrestricted -> the
        // other one alone; both unrestricted -> every day.
        let day_ok = if self.doms.star && self.dows.star {
            true
        } else if self.doms.star {
            self.dows.matches(f.dow)
        } else if self.dows.star {
            self.doms.matches(f.day)
        } else {
            self.doms.matches(f.day) || self.dows.matches(f.dow)
        };
        self.minutes.matches(f.minute)
            && self.hours.matches(f.hour)
            && self.months.matches(f.month)
            && day_ok
    }

    /// The first matching minute strictly after "from", scanning forward one minute at a time.
    /// None when nothing matches within a year (e.g. "0 0 30 2 *").
    pub fn next_after(&self, from: &chrono::NaiveDateTime) -> Option<chrono::NaiveDateTime> {
        let minute = TimeDelta::try_minutes(1)?;
        let mut t = from.checked_add_signed(minute)?;
        // 366 days covers a leap-year February without promising a match that does not exist.
        for _ in 0..(366 * 24 * 60) {
            if self.matches(&fields_of(&t)) {
                return Some(t);
            }
            t = t.checked_add_signed(minute)?;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(y: i32, m: u32, d: u32, h: u32, mi: u32) -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .expect("a real date")
            .and_hms_opt(h, mi, 0)
            .expect("a real time")
    }

    fn fires(expr: &str, t: chrono::NaiveDateTime) -> bool {
        CronExpr::parse(expr)
            .expect("a valid expression")
            .matches(&fields_of(&t))
    }

    #[test]
    fn a_daily_expression_fires_only_at_its_minute() {
        assert!(fires("30 3 * * *", at(2026, 9, 8, 3, 30)));
        assert!(!fires("30 3 * * *", at(2026, 9, 8, 3, 31)));
        assert!(!fires("30 3 * * *", at(2026, 9, 8, 15, 30)));
    }

    #[test]
    fn steps_ranges_and_lists_all_parse() {
        // */15 -> 0,15,30,45
        assert!(fires("*/15 * * * *", at(2026, 1, 1, 5, 45)));
        assert!(!fires("*/15 * * * *", at(2026, 1, 1, 5, 46)));
        // a-b/step -> exactly the stepped members of the range
        assert!(fires("10-30/10 * * * *", at(2026, 1, 1, 0, 20)));
        assert!(!fires("10-30/10 * * * *", at(2026, 1, 1, 0, 25)));
        // n/step -> n..max/step (vixie's open-ended shorthand)
        assert!(fires("5/15 * * * *", at(2026, 1, 1, 0, 35)));
        assert!(!fires("5/15 * * * *", at(2026, 1, 1, 0, 10)));
        // comma list
        assert!(fires("0 0 1,15 * *", at(2026, 1, 15, 0, 0)));
        assert!(!fires("0 0 1,15 * *", at(2026, 1, 16, 0, 0)));
        // month field
        assert!(fires("0 0 1 2 *", at(2026, 2, 1, 0, 0)));
        assert!(!fires("0 0 1 2 *", at(2026, 3, 1, 0, 0)));
    }

    #[test]
    fn both_day_fields_restricted_means_or_not_and() {
        // "day 1 OR Monday" - the 1st of September 2026 is a Tuesday, but vixie-cron still
        // fires because the dom matches; the AND reading would not.
        assert_eq!(at(2026, 9, 1, 0, 0).weekday().num_days_from_sunday(), 2);
        assert!(fires("0 0 1 * 1", at(2026, 9, 1, 0, 0)));
        // And a Monday that is not the 1st fires too (dow matches).
        assert!(fires("0 0 1 * 1", at(2026, 9, 7, 0, 0)));
        // Neither matches: the 2nd, a Wednesday.
        assert!(!fires("0 0 1 * 1", at(2026, 9, 2, 0, 0)));
    }

    #[test]
    fn one_restricted_day_field_stands_alone() {
        // dom is *, so the dow field decides: Sunday the 6th of September 2026.
        assert!(fires("0 0 * * 0", at(2026, 9, 6, 0, 0)));
        assert!(fires("0 0 * * 7", at(2026, 9, 6, 0, 0)), "7 is Sunday too");
        assert!(!fires("0 0 * * 1", at(2026, 9, 6, 0, 0)));
        // dow is *, so dom decides.
        assert!(fires("0 0 13 * *", at(2026, 9, 13, 0, 0)));
        assert!(!fires("0 0 13 * *", at(2026, 9, 14, 0, 0)));
    }

    #[test]
    fn malformed_expressions_are_rejected_with_a_reason() {
        for bad in [
            "",
            "* * * *",
            "* * * * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * 32 * *",
            "* * * 0 *",
            "* * * 13 *",
            "* * * * 8",
            "5-2 * * * *",
            "a * * * *",
            "*/0 * * * *",
            "1,,2 * * * *",
            "5/0 * * * *",
        ] {
            assert!(CronExpr::parse(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn next_after_finds_the_next_matching_minute() {
        let e = CronExpr::parse("30 3 * * *").expect("valid");
        // Later the same day -> tomorrow.
        assert_eq!(
            e.next_after(&at(2026, 9, 8, 10, 0)),
            Some(at(2026, 9, 9, 3, 30))
        );
        // Before it -> the same day.
        assert_eq!(
            e.next_after(&at(2026, 9, 8, 1, 0)),
            Some(at(2026, 9, 8, 3, 30))
        );
        // Strictly after: sitting on the matching minute does not fire again this minute.
        assert_eq!(
            e.next_after(&at(2026, 9, 8, 3, 30)),
            Some(at(2026, 9, 9, 3, 30))
        );
    }

    #[test]
    fn next_after_crosses_month_boundaries_and_gives_up_after_a_year() {
        let monthly = CronExpr::parse("0 0 1 * *").expect("valid");
        assert_eq!(
            monthly.next_after(&at(2026, 1, 2, 0, 0)),
            Some(at(2026, 2, 1, 0, 0))
        );
        // February 30th never exists - the scan must terminate with None, not loop forever.
        let never = CronExpr::parse("0 0 30 2 *").expect("valid");
        assert_eq!(never.next_after(&at(2026, 1, 1, 0, 0)), None);
    }

    #[test]
    fn star_steps_count_as_restricted_for_the_day_rule() {
        // "*/2" in dom is not a bare "*", so with a dow list the OR applies. Over 1..=31 the
        // step lands on ODD days: the 3rd matches dom. The 14th is an even Monday - dom does
        // not match, dow (1 = Monday) does, and the OR fires. The 2nd (even Wednesday) fails both.
        assert!(fires("0 0 */2 * 1", at(2026, 9, 3, 0, 0)));
        assert!(fires("0 0 */2 * 1", at(2026, 9, 14, 0, 0)));
        assert!(!fires("0 0 */2 * 1", at(2026, 9, 2, 0, 0)));
    }
}
