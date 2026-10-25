//! Запомняне и възстановяване на позиция и размер на прозорците.
//! Чиста логика без зависимост от Tauri, за да може да се тества отделно (`rustc --test`).
//! Всички координати са във физически пиксели, както ги дава Tauri.

#![allow(dead_code)]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }
    fn right(&self) -> i64 {
        self.x as i64 + self.w as i64
    }
    fn bottom(&self) -> i64 {
        self.y as i64 + self.h as i64
    }
}

/// Запазено състояние на един прозорец.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Saved {
    /// Обикновена (непразна, неувеличена) позиция и размер.
    pub rect: Rect,
    pub maximized: bool,
    pub fullscreen: bool,
    /// Монитор, на който е бил прозорецът (за да познаем дали още е закачен).
    pub monitor: Option<Rect>,
}

/// Какво да направим при отваряне.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    /// None = оставяме системата да избере (центриран по подразбиране).
    pub rect: Option<Rect>,
    pub maximized: bool,
    pub fullscreen: bool,
    /// Монитор, на който ще се покаже (нужен за цял екран).
    pub monitor: Option<Rect>,
}

/// Най-малката видима част, която приемаме за „прозорецът е достижим“.
const MIN_VISIBLE_W: i64 = 120;
const MIN_VISIBLE_H: i64 = 60;

fn overlap(a: &Rect, b: &Rect) -> (i64, i64) {
    let w = a.right().min(b.right()) - (a.x as i64).max(b.x as i64);
    let h = a.bottom().min(b.bottom()) - (a.y as i64).max(b.y as i64);
    (w.max(0), h.max(0))
}

fn area(a: &Rect, b: &Rect) -> i64 {
    let (w, h) = overlap(a, b);
    w * h
}

/// Монитор, който най-много се застъпва с прозореца, и индексът му.
pub fn best_monitor(win: &Rect, mons: &[Rect]) -> Option<usize> {
    let mut best: Option<(usize, i64)> = None;
    for (i, m) in mons.iter().enumerate() {
        let a = area(win, m);
        if a > 0 && best.map(|(_, b)| a > b).unwrap_or(true) {
            best = Some((i, a));
        }
    }
    best.map(|(i, _)| i)
}

/// Достатъчно ли се вижда прозорецът на някой от мониторите (да не е останал извън екрана).
pub fn visible(win: &Rect, mons: &[Rect]) -> bool {
    mons.iter().any(|m| {
        let (w, h) = overlap(win, m);
        w >= MIN_VISIBLE_W.min(win.w as i64) && h >= MIN_VISIBLE_H.min(win.h as i64)
    })
}

/// Свива и премества прозореца така, че да се побере изцяло в монитора.
pub fn fit(win: &Rect, mon: &Rect) -> Rect {
    let w = win.w.min(mon.w).max(1);
    let h = win.h.min(mon.h).max(1);
    let max_x = mon.x as i64 + mon.w as i64 - w as i64;
    let max_y = mon.y as i64 + mon.h as i64 - h as i64;
    let x = (win.x as i64).clamp(mon.x as i64, max_x.max(mon.x as i64));
    let y = (win.y as i64).clamp(mon.y as i64, max_y.max(mon.y as i64));
    Rect { x: x as i32, y: y as i32, w, h }
}

/// Прозорец с размер `size`, центриран в монитора (и смален, ако не се побира).
pub fn centered(size: (u32, u32), mon: &Rect) -> Rect {
    let w = size.0.min(mon.w).max(1);
    let h = size.1.min(mon.h).max(1);
    let x = mon.x as i64 + (mon.w as i64 - w as i64) / 2;
    let y = mon.y as i64 + (mon.h as i64 - h as i64) / 2;
    Rect { x: x as i32, y: y as i32, w, h }
}

/// Главен прозорец: връщаме последното място, ако още се вижда; иначе None (системен избор).
pub fn plan_main(saved: Option<&Saved>, mons: &[Rect]) -> Option<Place> {
    let s = saved?;
    if mons.is_empty() || !visible(&s.rect, mons) {
        return None;
    }
    let i = best_monitor(&s.rect, mons)?;
    Some(Place {
        rect: Some(fit(&s.rect, &mons[i])),
        maximized: s.maximized,
        fullscreen: false,
        monitor: Some(mons[i]),
    })
}

fn same(a: &Rect, b: &Rect) -> bool {
    a == b
}

/// Втори прозорец (за четене).
///  - Ако е закачен друг монитор (различен от този на главния прозорец):
///      * ако последния път е бил на точно такъв монитор, връщаме запазеното състояние там;
///      * иначе се отваря на цял екран на другия монитор.
///  - Ако няма втори монитор: отваря се на основния (монитора на главния прозорец), в прозорец,
///    с последния размер, ако е бил запазен на същия монитор; иначе по подразбиране.
pub fn plan_reader(saved: Option<&Saved>, mons: &[Rect], main_mon: &Rect, default: (u32, u32)) -> Place {
    let other = mons.iter().find(|m| !same(m, main_mon)).copied();

    if let Some(o) = other {
        if let Some(s) = saved {
            if let Some(sm) = s.monitor {
                // запазеният монитор трябва още да е закачен и да не е този на главния прозорец
                if !same(&sm, main_mon) {
                    if let Some(m) = mons.iter().find(|m| same(m, &sm)) {
                        let rect = if visible(&s.rect, std::slice::from_ref(m)) {
                            fit(&s.rect, m)
                        } else {
                            centered(default, m)
                        };
                        return Place { rect: Some(rect), maximized: false, fullscreen: s.fullscreen, monitor: Some(*m) };
                    }
                }
            }
        }
        return Place { rect: Some(centered(default, &o)), maximized: false, fullscreen: true, monitor: Some(o) };
    }

    // няма втори монитор -> основния
    let rect = match saved {
        Some(s) if s.monitor.map(|m| same(&m, main_mon)).unwrap_or(false) && visible(&s.rect, std::slice::from_ref(main_mon)) => {
            fit(&s.rect, main_mon)
        }
        _ => centered(default, main_mon),
    };
    Place { rect: Some(rect), maximized: false, fullscreen: false, monitor: Some(*main_mon) }
}

// ---------- запис във файл (JSON) без външни зависимости ----------

fn num(s: &str, key: &str) -> Option<i64> {
    let k = format!("\"{}\":", key);
    let i = s.find(&k)? + k.len();
    let rest = &s[i..];
    let end = rest.find(|c: char| !(c == '-' || c.is_ascii_digit())).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn flag(s: &str, key: &str) -> bool {
    s.contains(&format!("\"{}\":true", key))
}

fn rect_json(r: &Rect) -> String {
    format!("{{\"x\":{},\"y\":{},\"w\":{},\"h\":{}}}", r.x, r.y, r.w, r.h)
}

fn parse_rect(s: &str) -> Option<Rect> {
    let w = num(s, "w")?;
    let h = num(s, "h")?;
    if w <= 0 || h <= 0 || w > 100_000 || h > 100_000 {
        return None;
    }
    Some(Rect { x: num(s, "x")? as i32, y: num(s, "y")? as i32, w: w as u32, h: h as u32 })
}

pub fn saved_to_json(s: &Saved) -> String {
    let mon = match &s.monitor {
        Some(m) => rect_json(m),
        None => "null".to_string(),
    };
    format!(
        "{{\"rect\":{},\"maximized\":{},\"fullscreen\":{},\"monitor\":{}}}",
        rect_json(&s.rect),
        s.maximized,
        s.fullscreen,
        mon
    )
}

/// Връща вътрешността на обект `"key":{...}` (без вложени обекти).
fn obj<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let k = format!("\"{}\":{{", key);
    let i = s.find(&k)? + k.len();
    let end = s[i..].find('}')?;
    Some(&s[i..i + end])
}

pub fn saved_from_json(s: &str) -> Option<Saved> {
    let rect = parse_rect(obj(s, "rect")?)?;
    let monitor = obj(s, "monitor").and_then(parse_rect);
    // флаговете търсим само извън вложените обекти
    let outer = {
        let mut t = s.to_string();
        for k in ["rect", "monitor"] {
            if let Some(o) = obj(&t, k).map(|x| x.to_string()) {
                t = t.replace(&o, "");
            }
        }
        t
    };
    Some(Saved { rect, maximized: flag(&outer, "maximized"), fullscreen: flag(&outer, "fullscreen"), monitor })
}

/// Файлът пази по един запис за всеки прозорец: {"main":{...},"reader":{...}}
pub fn file_to_json(entries: &[(&str, Saved)]) -> String {
    let parts: Vec<String> = entries.iter().map(|(k, v)| format!("\"{}\":{}", k, saved_to_json(v))).collect();
    format!("{{{}}}", parts.join(","))
}

pub fn file_get(text: &str, key: &str) -> Option<Saved> {
    // записът на прозореца започва след "key":{ и завършва преди следващия ключ на най-горно ниво
    let k = format!("\"{}\":{{", key);
    let i = text.find(&k)? + k.len() - 1;
    let mut depth = 0i32;
    let bytes = text.as_bytes();
    let mut j = i;
    while j < bytes.len() {
        match bytes[j] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return saved_from_json(&text[i..=j]);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Rect = Rect { x: 0, y: 0, w: 1920, h: 1080 };
    const B: Rect = Rect { x: 1920, y: 0, w: 2560, h: 1440 };
    const LEFT: Rect = Rect { x: -1280, y: 0, w: 1280, h: 1024 };

    fn sv(rect: Rect, fs: bool, mon: Option<Rect>) -> Saved {
        Saved { rect, maximized: false, fullscreen: fs, monitor: mon }
    }

    #[test]
    fn main_restores_when_visible() {
        let s = sv(Rect::new(100, 80, 1200, 800), false, Some(A));
        let p = plan_main(Some(&s), &[A]).unwrap();
        assert_eq!(p.rect, Some(s.rect));
    }

    #[test]
    fn main_on_unplugged_monitor_falls_back() {
        // последно е бил на втория монитор, който вече не е закачен
        let s = sv(Rect::new(2500, 100, 1200, 800), false, Some(B));
        assert_eq!(plan_main(Some(&s), &[A]), None);
    }

    #[test]
    fn main_on_negative_monitor() {
        let s = sv(Rect::new(-1000, 50, 900, 700), false, Some(LEFT));
        let p = plan_main(Some(&s), &[LEFT, A]).unwrap();
        assert_eq!(p.rect, Some(s.rect));
    }

    #[test]
    fn main_too_big_is_fitted() {
        let s = sv(Rect::new(-50, -30, 3000, 2000), false, Some(A));
        let p = plan_main(Some(&s), &[A]).unwrap();
        assert_eq!(p.rect, Some(Rect::new(0, 0, 1920, 1080)));
    }

    #[test]
    fn main_mostly_offscreen_is_ignored() {
        let s = sv(Rect::new(1900, 1000, 1200, 800), false, Some(A));
        assert_eq!(plan_main(Some(&s), &[A]), None);
    }

    #[test]
    fn main_keeps_maximized() {
        let mut s = sv(Rect::new(100, 80, 1200, 800), false, Some(A));
        s.maximized = true;
        assert!(plan_main(Some(&s), &[A]).unwrap().maximized);
    }

    #[test]
    fn reader_first_time_goes_fullscreen_on_second() {
        let p = plan_reader(None, &[A, B], &A, (1280, 720));
        assert!(p.fullscreen);
        assert_eq!(p.monitor, Some(B));
        let r = p.rect.unwrap();
        assert!(r.x >= B.x && (r.x as i64 + r.w as i64) <= B.right());
    }

    #[test]
    fn reader_remembered_windowed_on_second() {
        let s = sv(Rect::new(2100, 100, 1000, 600), false, Some(B));
        let p = plan_reader(Some(&s), &[A, B], &A, (1280, 720));
        assert!(!p.fullscreen);
        assert_eq!(p.rect, Some(s.rect));
        assert_eq!(p.monitor, Some(B));
    }

    #[test]
    fn reader_remembered_fullscreen_on_second() {
        let s = sv(Rect::new(2100, 100, 1000, 600), true, Some(B));
        let p = plan_reader(Some(&s), &[A, B], &A, (1280, 720));
        assert!(p.fullscreen);
        assert_eq!(p.monitor, Some(B));
    }

    #[test]
    fn reader_second_monitor_unplugged_opens_on_main() {
        let s = sv(Rect::new(2100, 100, 1000, 600), true, Some(B));
        let p = plan_reader(Some(&s), &[A], &A, (1280, 720));
        assert!(!p.fullscreen);
        assert_eq!(p.monitor, Some(A));
        let r = p.rect.unwrap();
        assert!(r.x >= 0 && r.y >= 0 && r.right() <= 1920 && r.bottom() <= 1080);
    }

    #[test]
    fn reader_remembered_on_main_when_alone() {
        let s = sv(Rect::new(300, 200, 900, 500), false, Some(A));
        let p = plan_reader(Some(&s), &[A], &A, (1280, 720));
        assert_eq!(p.rect, Some(s.rect));
        assert!(!p.fullscreen);
    }

    #[test]
    fn reader_saved_on_main_but_second_plugged_now_goes_to_second() {
        let s = sv(Rect::new(300, 200, 900, 500), false, Some(A));
        let p = plan_reader(Some(&s), &[A, B], &A, (1280, 720));
        assert!(p.fullscreen);
        assert_eq!(p.monitor, Some(B));
    }

    #[test]
    fn reader_second_monitor_changed_resolution_is_treated_as_new() {
        let b2 = Rect::new(1920, 0, 1920, 1080);
        let s = sv(Rect::new(2100, 100, 1000, 600), false, Some(B));
        let p = plan_reader(Some(&s), &[A, b2], &A, (1280, 720));
        assert!(p.fullscreen);
        assert_eq!(p.monitor, Some(b2));
    }

    #[test]
    fn reader_main_window_on_second_monitor_goes_to_first() {
        // главният прозорец е на монитор B, значи „другият“ е A
        let p = plan_reader(None, &[A, B], &B, (1280, 720));
        assert_eq!(p.monitor, Some(A));
        assert!(p.fullscreen);
    }

    #[test]
    fn reader_saved_rect_offscreen_gets_centered() {
        let s = sv(Rect::new(9000, 9000, 1000, 600), false, Some(B));
        let p = plan_reader(Some(&s), &[A, B], &A, (1280, 720));
        let r = p.rect.unwrap();
        assert!(r.x >= B.x && r.right() <= B.right() && r.bottom() <= B.bottom());
    }

    #[test]
    fn json_roundtrip() {
        let s = Saved { rect: Rect::new(-1000, 50, 900, 700), maximized: true, fullscreen: false, monitor: Some(LEFT) };
        assert_eq!(saved_from_json(&saved_to_json(&s)), Some(s));
        let n = Saved { rect: Rect::new(1, 2, 3, 4), maximized: false, fullscreen: true, monitor: None };
        assert_eq!(saved_from_json(&saved_to_json(&n)), Some(n));
    }

    #[test]
    fn file_roundtrip_and_garbage() {
        let m = sv(Rect::new(10, 20, 1200, 800), false, Some(A));
        let r = Saved { rect: Rect::new(2100, 0, 800, 600), maximized: false, fullscreen: true, monitor: Some(B) };
        let t = file_to_json(&[("main", m), ("reader", r)]);
        assert_eq!(file_get(&t, "main"), Some(m));
        assert_eq!(file_get(&t, "reader"), Some(r));
        assert_eq!(file_get(&t, "other"), None);
        assert_eq!(file_get("", "main"), None);
        assert_eq!(file_get("{\"main\":{\"rect\":{\"x\":1,\"y\":1,\"w\":0,\"h\":5}}}", "main"), None);
        assert_eq!(file_get("not json at all", "main"), None);
    }
}
