//! Breakout2D MVP v2: arena top-down, hasta 4 humanos + bots, extraccion,
//! armas con cargador y recarga, municion limitada, saqueo de cadaveres y mochila.
//! Un solo ejecutable:  breakout2d.exe host   |   breakout2d.exe join <ip>[:puerto]
//! El host simula todo (autoritativo); los clientes mandan input y dibujan el estado.

use breakout2d_core::*;
use macroquad::prelude::*;
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};

const PORT: u16 = 7777;
const W: f32 = 1600.0;
const H: f32 = 1000.0;
const MAX_PLAYERS: usize = 4;
const BOTS: usize = 3;
const R: f32 = 14.0;
const SPEED: f32 = 190.0;
const RANGE: f32 = 700.0;
const ARMOR_DUR: f32 = 60.0;
const MAX_SLOTS: usize = 8;
const CORPSE_SLOTS: usize = 16;

const ACT_RELOAD: u8 = 1;
const ACT_LOOT: u8 = 2;
const ACT_SWAP: u8 = 4;

static AMMO: [Ammo; 2] = [
    Ammo { name: "FMJ", damage: 40.0, penetration: 20.0, armor_damage: 8.0 },
    Ammo { name: "AP", damage: 55.0, penetration: 55.0, armor_damage: 14.0 },
];

struct Wpn { name: &'static str, mag: u16, rate: f32, reload: f32, dmg_mult: f32 }
static WPNS: [Wpn; 2] = [
    Wpn { name: "Pistola", mag: 12, rate: 0.22, reload: 1.4, dmg_mult: 0.8 },
    Wpn { name: "Fusil", mag: 30, rate: 0.10, reload: 2.4, dmg_mult: 1.0 },
];

const WALLS: [(f32, f32, f32, f32); 5] = [
    (400.0, 200.0, 40.0, 400.0),
    (800.0, 500.0, 400.0, 40.0),
    (1100.0, 100.0, 40.0, 350.0),
    (300.0, 700.0, 500.0, 40.0),
    (1000.0, 750.0, 40.0, 250.0),
];
const EXTRACT: (f32, f32, f32, f32) = (1450.0, 420.0, 120.0, 160.0);

// ---------- items / mochila ----------
// kind: 0 = municion FMJ, 1 = municion AP, 2 = pistola, 3 = fusil, 4 = objeto valioso
#[derive(Clone, Copy, Debug)]
struct Item { kind: u8, qty: u16 }

fn stack_max(kind: u8) -> u16 {
    match kind { 0 | 1 => 60, 4 => 20, _ => 1 }
}

fn item_name(kind: u8) -> &'static str {
    match kind {
        0 => "Municion FMJ",
        1 => "Municion AP",
        2 => "Pistola",
        3 => "Fusil",
        _ => "Objeto valioso",
    }
}

/// Agrega al inventario; devuelve lo que NO cupo.
fn add_item(inv: &mut Vec<Item>, kind: u8, qty: u16, max_slots: usize) -> u16 {
    let mut left = qty;
    let cap = stack_max(kind);
    for it in inv.iter_mut() {
        if left == 0 { break; }
        if it.kind == kind && it.qty < cap {
            let t = (cap - it.qty).min(left);
            it.qty += t;
            left -= t;
        }
    }
    while left > 0 && inv.len() < max_slots {
        let t = cap.min(left);
        inv.push(Item { kind, qty: t });
        left -= t;
    }
    left
}

fn count_item(inv: &[Item], kind: u8) -> u16 {
    inv.iter().filter(|i| i.kind == kind).map(|i| i.qty).sum()
}

/// Saca hasta `want` unidades; devuelve cuantas saco.
fn take_item(inv: &mut Vec<Item>, kind: u8, want: u16) -> u16 {
    let mut got = 0u16;
    for it in inv.iter_mut() {
        if got >= want { break; }
        if it.kind == kind {
            let t = it.qty.min(want - got);
            it.qty -= t;
            got += t;
        }
    }
    inv.retain(|i| i.qty > 0);
    got
}

// ---------- utilidades ----------
struct Rng(u64);
impl Rng {
    fn f(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 40) as f32) / 16777216.0
    }
}

fn blocked(x: f32, y: f32) -> bool {
    if x < R || y < R || x > W - R || y > H - R {
        return true;
    }
    WALLS.iter().any(|&(wx, wy, ww, wh)| {
        let cx = x.clamp(wx, wx + ww);
        let cy = y.clamp(wy, wy + wh);
        (x - cx).powi(2) + (y - cy).powi(2) < R * R
    })
}

fn blocked_point(x: f32, y: f32) -> bool {
    x < 0.0 || y < 0.0 || x > W || y > H
        || WALLS.iter().any(|&(wx, wy, ww, wh)| x >= wx && x <= wx + ww && y >= wy && y <= wy + wh)
}

fn in_extract(x: f32, y: f32) -> bool {
    x >= EXTRACT.0 && x <= EXTRACT.0 + EXTRACT.2 && y >= EXTRACT.1 && y <= EXTRACT.1 + EXTRACT.3
}

// ---------- simulacion (host) ----------
struct Ent {
    x: f32, y: f32, aim: f32,
    body: Body, armor: Armor,
    alive: bool, extracted: bool, bot: bool, active: bool,
    cd: f32, extract_t: f32,
    keys: u8, shoot: bool, act: u8, prev_act: u8,
    wpn: Option<u8>, mag: u16, mag_kind: u8, sel: u8, reload_t: f32,
    inv: Vec<Item>,
}

struct Corpse { x: f32, y: f32, items: Vec<Item> }

struct World {
    ents: Vec<Ent>,
    corpses: Vec<Corpse>,
    tracers: Vec<(f32, f32, f32, f32)>,
    rng: Rng,
}

fn make_ent(idx: usize, rng: &mut Rng) -> Ent {
    let bot = idx >= MAX_PLAYERS;
    let (x, y) = if bot {
        (1250.0 + rng.f() * 150.0, 100.0 + rng.f() * 800.0)
    } else {
        (100.0, 150.0 + idx as f32 * 200.0)
    };
    let class = if bot { 2 + (rng.f() * 3.0) as u8 } else { 3 };

    let mut inv: Vec<Item> = Vec::new();
    let wpn: Option<u8>;
    let mag: u16;
    let mag_kind: u8;
    let sel: u8;
    if bot {
        let w: u8 = if rng.f() > 0.4 { 1 } else { 0 };
        let k: u8 = if rng.f() > 0.65 { 1 } else { 0 };
        wpn = Some(w);
        mag = WPNS[w as usize].mag;
        mag_kind = k;
        sel = k;
        add_item(&mut inv, k, 10 + (rng.f() * 30.0) as u16, MAX_SLOTS);
        if rng.f() > 0.5 { add_item(&mut inv, 4, 1 + (rng.f() * 2.0) as u16, MAX_SLOTS); }
        if rng.f() > 0.6 { add_item(&mut inv, 2 + (1 - w), 1, MAX_SLOTS); }
    } else {
        wpn = Some(0);
        mag = WPNS[0].mag;
        mag_kind = 0;
        sel = 0;
        add_item(&mut inv, 0, 24, MAX_SLOTS);
        add_item(&mut inv, 1, 10, MAX_SLOTS);
    }

    Ent {
        x, y, aim: 0.0,
        body: Body::new(),
        armor: Armor::new(class, ARMOR_DUR, 0.3, vec![Zone::Thorax, Zone::Stomach]),
        alive: true, extracted: false, bot, active: bot || idx == 0,
        cd: 0.0, extract_t: 0.0,
        keys: 0, shoot: false, act: 0, prev_act: 0,
        wpn, mag, mag_kind, sel, reload_t: 0.0,
        inv,
    }
}

impl World {
    fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(88172645463325252)
            | 1;
        let mut rng = Rng(seed);
        let ents = (0..MAX_PLAYERS + BOTS).map(|i| make_ent(i, &mut rng)).collect();
        World { ents, corpses: vec![], tracers: vec![], rng }
    }
    fn reset(&mut self) {
        for i in 0..self.ents.len() {
            let was = self.ents[i].active;
            let mut e = make_ent(i, &mut self.rng);
            e.active = e.bot || was;
            self.ents[i] = e;
        }
        self.corpses.clear();
    }
}

// ---------- recarga / armas / saqueo ----------
fn start_reload(e: &mut Ent) {
    let Some(wi) = e.wpn else { return };
    if e.reload_t > 0.0 { return; }
    let wi = (wi as usize).min(1);
    let cap = WPNS[wi].mag;
    // los bots cambian solos al otro tipo de municion si se les acabo
    if e.bot && count_item(&e.inv, e.sel) == 0 && count_item(&e.inv, 1 - e.sel.min(1)) > 0 {
        e.sel = 1 - e.sel.min(1);
    }
    if count_item(&e.inv, e.sel) == 0 { return; }
    if e.mag_kind == e.sel && e.mag >= cap { return; }
    e.reload_t = WPNS[wi].reload;
}

fn finish_reload(e: &mut Ent) {
    let Some(wi) = e.wpn else { return };
    let cap = WPNS[(wi as usize).min(1)].mag;
    if e.mag > 0 && e.mag_kind != e.sel {
        // devolver las balas del cargador a la mochila antes de cambiar de tipo
        let left = add_item(&mut e.inv, e.mag_kind, e.mag, MAX_SLOTS);
        if left > 0 { e.mag = left; return; } // mochila llena: cancelar
        e.mag = 0;
    }
    if e.mag == 0 { e.mag_kind = e.sel; }
    let got = take_item(&mut e.inv, e.sel, cap.saturating_sub(e.mag));
    e.mag += got;
}

fn swap_weapon(e: &mut Ent) {
    let Some(pos) = e.inv.iter().position(|i| i.kind == 2 || i.kind == 3) else { return };
    if e.mag > 0 {
        let left = add_item(&mut e.inv, e.mag_kind, e.mag, MAX_SLOTS);
        if left > 0 { e.mag = left; return; }
        e.mag = 0;
    }
    let new_w = e.inv[pos].kind - 2;
    match e.wpn {
        Some(old) => { e.inv[pos].kind = 2 + old; }
        None => { e.inv.remove(pos); }
    }
    e.wpn = Some(new_w);
    e.reload_t = 0.0;
}

fn make_corpse(w: &mut World, j: usize) {
    let e = &mut w.ents[j];
    let mut items: Vec<Item> = Vec::new();
    for it in e.inv.drain(..) {
        add_item(&mut items, it.kind, it.qty, CORPSE_SLOTS);
    }
    if let Some(wi) = e.wpn.take() {
        add_item(&mut items, 2 + wi, 1, CORPSE_SLOTS);
    }
    if e.mag > 0 {
        add_item(&mut items, e.mag_kind, e.mag, CORPSE_SLOTS);
        e.mag = 0;
    }
    let (x, y) = (e.x, e.y);
    if !items.is_empty() {
        w.corpses.push(Corpse { x, y, items });
    }
}

fn loot(w: &mut World, i: usize) {
    let (px, py) = (w.ents[i].x, w.ents[i].y);
    let mut best: Option<usize> = None;
    let mut bd = 50.0f32;
    for (k, c) in w.corpses.iter().enumerate() {
        let d = ((c.x - px).powi(2) + (c.y - py).powi(2)).sqrt();
        if d < bd { bd = d; best = Some(k); }
    }
    let Some(k) = best else { return };
    let items = std::mem::take(&mut w.corpses[k].items);
    let mut rest: Vec<Item> = Vec::new();
    for it in items {
        let left = add_item(&mut w.ents[i].inv, it.kind, it.qty, MAX_SLOTS);
        if left > 0 { rest.push(Item { kind: it.kind, qty: left }); }
    }
    w.corpses[k].items = rest;
    if w.corpses[k].items.is_empty() { w.corpses.remove(k); }
}

// ---------- IA y combate ----------
fn bot_ai(w: &mut World, i: usize) {
    if w.ents[i].mag == 0 { start_reload(&mut w.ents[i]); }
    let (bx, by) = (w.ents[i].x, w.ents[i].y);
    let mut best: Option<usize> = None;
    let mut bd = 500.0f32;
    for j in 0..MAX_PLAYERS {
        let e = &w.ents[j];
        if e.active && e.alive && !e.extracted {
            let d = ((e.x - bx).powi(2) + (e.y - by).powi(2)).sqrt();
            if d < bd { bd = d; best = Some(j); }
        }
    }
    let spread = (w.rng.f() - 0.5) * 0.12;
    let wander = w.rng.f();
    let rand_keys = (w.rng.f() * 16.0) as u8;
    match best {
        Some(j) => {
            let (tx, ty) = (w.ents[j].x, w.ents[j].y);
            let mut keys = 0u8;
            if bd > 220.0 {
                if ty < by - 8.0 { keys |= 1; } else if ty > by + 8.0 { keys |= 2; }
                if tx < bx - 8.0 { keys |= 4; } else if tx > bx + 8.0 { keys |= 8; }
            }
            let e = &mut w.ents[i];
            e.aim = (ty - by).atan2(tx - bx) + spread;
            e.shoot = bd < 450.0;
            e.keys = keys;
        }
        None => {
            let e = &mut w.ents[i];
            e.shoot = false;
            if wander < 0.02 { e.keys = rand_keys; }
        }
    }
}

fn fire(w: &mut World, i: usize) {
    let (ox, oy, aim) = (w.ents[i].x, w.ents[i].y, w.ents[i].aim);
    let wi = (w.ents[i].wpn.unwrap_or(0) as usize).min(1);
    let ak = (w.ents[i].mag_kind as usize).min(1);
    let base = &AMMO[ak];
    let ammo = Ammo {
        name: base.name,
        damage: base.damage * WPNS[wi].dmg_mult,
        penetration: base.penetration,
        armor_damage: base.armor_damage,
    };
    let (dx, dy) = (aim.cos(), aim.sin());
    let mut end = (ox + dx * RANGE, oy + dy * RANGE);
    let mut d = R + 2.0;
    'ray: while d < RANGE {
        let (px, py) = (ox + dx * d, oy + dy * d);
        if blocked_point(px, py) { end = (px, py); break; }
        for j in 0..w.ents.len() {
            if j == i { continue; }
            let (ok, jx, jy) = {
                let e = &w.ents[j];
                (e.active && e.alive && !e.extracted, e.x, e.y)
            };
            if !ok { continue; }
            if (px - jx).powi(2) + (py - jy).powi(2) < R * R {
                let zr = w.rng.f();
                let zone = if zr < 0.10 { Zone::Head } else if zr < 0.55 { Zone::Thorax }
                    else if zr < 0.70 { Zone::Stomach } else if zr < 0.85 { Zone::Arm } else { Zone::Leg };
                let roll = w.rng.f();
                let res = resolve_hit(&ammo, zone, Some(&mut w.ents[j].armor), roll);
                w.ents[j].body.hit(zone, res.body_damage);
                // simplificacion MVP: dano en vientre/extremidades tambien pasa al torso
                if !matches!(zone, Zone::Head | Zone::Thorax) {
                    w.ents[j].body.hit(Zone::Thorax, res.body_damage * 0.35);
                }
                if w.ents[j].body.is_dead() {
                    w.ents[j].alive = false;
                    make_corpse(w, j);
                }
                end = (px, py);
                break 'ray;
            }
        }
        d += 4.0;
    }
    w.tracers.push((ox, oy, end.0, end.1));
}

fn step(w: &mut World, dt: f32) {
    for i in 0..w.ents.len() {
        if !w.ents[i].active || !w.ents[i].alive || w.ents[i].extracted { continue; }
        if w.ents[i].bot { bot_ai(w, i); }

        // movimiento
        let keys = w.ents[i].keys;
        let (mut dx, mut dy) = (0.0f32, 0.0f32);
        if keys & 1 != 0 { dy -= 1.0; }
        if keys & 2 != 0 { dy += 1.0; }
        if keys & 4 != 0 { dx -= 1.0; }
        if keys & 8 != 0 { dx += 1.0; }
        let len = (dx * dx + dy * dy).sqrt();
        if len > 0.0 { dx /= len; dy /= len; }
        let sp = if w.ents[i].bot { SPEED * 0.8 } else { SPEED };
        let nx = w.ents[i].x + dx * sp * dt;
        if !blocked(nx, w.ents[i].y) { w.ents[i].x = nx; }
        let ny = w.ents[i].y + dy * sp * dt;
        if !blocked(w.ents[i].x, ny) { w.ents[i].y = ny; }

        // recarga en curso + acciones (flanco de subida)
        let rising = {
            let e = &mut w.ents[i];
            if e.reload_t > 0.0 {
                e.reload_t -= dt;
                if e.reload_t <= 0.0 { e.reload_t = 0.0; finish_reload(e); }
            }
            let r = e.act & !e.prev_act;
            e.prev_act = e.act;
            if r & ACT_RELOAD != 0 { start_reload(e); }
            if r & ACT_SWAP != 0 { swap_weapon(e); }
            r
        };
        if rising & ACT_LOOT != 0 { loot(w, i); }

        // disparo
        w.ents[i].cd -= dt;
        let can = w.ents[i].shoot && w.ents[i].cd <= 0.0
            && w.ents[i].reload_t <= 0.0 && w.ents[i].wpn.is_some();
        if can {
            if w.ents[i].mag == 0 {
                start_reload(&mut w.ents[i]);
            } else {
                let bot = w.ents[i].bot;
                let wi = (w.ents[i].wpn.unwrap_or(0) as usize).min(1);
                w.ents[i].cd = WPNS[wi].rate * if bot { 2.5 } else { 1.0 };
                w.ents[i].mag -= 1;
                fire(w, i);
            }
        }

        // extraccion
        if !w.ents[i].bot {
            if in_extract(w.ents[i].x, w.ents[i].y) {
                w.ents[i].extract_t += dt;
                if w.ents[i].extract_t >= 3.0 { w.ents[i].extracted = true; }
            } else {
                w.ents[i].extract_t = 0.0;
            }
        }
    }
}

// ---------- red ----------
#[derive(Clone, Default)]
struct View {
    active: bool, alive: bool, extracted: bool, bot: bool,
    x: f32, y: f32, aim: f32, hp: f32, armor: f32,
}
#[derive(Clone, Default)]
struct CorpseView { x: f32, y: f32, items: Vec<Item> }
#[derive(Clone, Default)]
struct MeState { wpn: Option<u8>, mag: u16, mag_kind: u8, sel: u8, reload_t: f32, inv: Vec<Item> }
#[derive(Clone, Default)]
struct Snapshot {
    you: usize,
    views: Vec<View>,
    tracers: Vec<(f32, f32, f32, f32)>,
    corpses: Vec<CorpseView>,
    mine: MeState,
}

impl Default for Item {
    fn default() -> Self { Item { kind: 0, qty: 0 } }
}

fn put_f32(b: &mut Vec<u8>, v: f32) { b.extend_from_slice(&v.to_le_bytes()); }
fn put_items(b: &mut Vec<u8>, items: &[Item], max: usize) {
    let k = items.len().min(max);
    b.push(k as u8);
    for it in items.iter().take(k) {
        b.push(it.kind);
        b.extend_from_slice(&it.qty.to_le_bytes());
    }
}

struct Rd<'a> { b: &'a [u8], o: usize }
impl<'a> Rd<'a> {
    fn u8(&mut self) -> Option<u8> { let v = *self.b.get(self.o)?; self.o += 1; Some(v) }
    fn u16(&mut self) -> Option<u16> { let a = self.u8()?; let c = self.u8()?; Some(u16::from_le_bytes([a, c])) }
    fn f32(&mut self) -> Option<f32> {
        let mut a = [0u8; 4];
        for k in 0..4 { a[k] = self.u8()?; }
        Some(f32::from_le_bytes(a))
    }
    fn items(&mut self) -> Option<Vec<Item>> {
        let k = self.u8()? as usize;
        let mut v = Vec::with_capacity(k);
        for _ in 0..k {
            let kind = self.u8()?;
            let qty = self.u16()?;
            v.push(Item { kind, qty });
        }
        Some(v)
    }
}

fn encode_state(w: &World, you: u8) -> Vec<u8> {
    let nt = w.tracers.len().min(16);
    let nc = w.corpses.len().min(8);
    let mut b = vec![3u8, you, w.ents.len() as u8, nt as u8, nc as u8];
    for e in &w.ents {
        b.push(e.active as u8);
        b.push((e.alive as u8) | ((e.extracted as u8) << 1) | ((e.bot as u8) << 2));
        put_f32(&mut b, e.x);
        put_f32(&mut b, e.y);
        put_f32(&mut b, e.aim);
        put_f32(&mut b, e.body.hp(Zone::Thorax));
        put_f32(&mut b, e.armor.durability);
    }
    for t in w.tracers.iter().take(nt) {
        put_f32(&mut b, t.0); put_f32(&mut b, t.1); put_f32(&mut b, t.2); put_f32(&mut b, t.3);
    }
    for c in w.corpses.iter().take(nc) {
        put_f32(&mut b, c.x);
        put_f32(&mut b, c.y);
        put_items(&mut b, &c.items, 16);
    }
    let e = &w.ents[(you as usize).min(w.ents.len() - 1)];
    b.push(e.wpn.unwrap_or(255));
    b.extend_from_slice(&e.mag.to_le_bytes());
    b.push(e.mag_kind);
    b.push(e.sel);
    put_f32(&mut b, e.reload_t);
    put_items(&mut b, &e.inv, MAX_SLOTS);
    b
}

fn decode_state(b: &[u8]) -> Option<Snapshot> {
    let mut r = Rd { b, o: 0 };
    if r.u8()? != 3 { return None; }
    let you = r.u8()? as usize;
    let n = r.u8()? as usize;
    let nt = r.u8()? as usize;
    let nc = r.u8()? as usize;
    let mut views = Vec::with_capacity(n);
    for _ in 0..n {
        let active = r.u8()? != 0;
        let fl = r.u8()?;
        let x = r.f32()?; let y = r.f32()?; let aim = r.f32()?;
        let hp = r.f32()?; let armor = r.f32()?;
        views.push(View { active, alive: fl & 1 != 0, extracted: fl & 2 != 0, bot: fl & 4 != 0,
                          x, y, aim, hp, armor });
    }
    let mut tracers = Vec::with_capacity(nt);
    for _ in 0..nt {
        let a = r.f32()?; let c = r.f32()?; let d = r.f32()?; let e = r.f32()?;
        tracers.push((a, c, d, e));
    }
    let mut corpses = Vec::with_capacity(nc);
    for _ in 0..nc {
        let x = r.f32()?; let y = r.f32()?;
        let items = r.items()?;
        corpses.push(CorpseView { x, y, items });
    }
    let wpn_raw = r.u8()?;
    let mag = r.u16()?;
    let mag_kind = r.u8()?;
    let sel = r.u8()?;
    let reload_t = r.f32()?;
    let inv = r.items()?;
    Some(Snapshot {
        you: you.min(n.saturating_sub(1)),
        views, tracers, corpses,
        mine: MeState {
            wpn: if wpn_raw == 255 { None } else { Some(wpn_raw) },
            mag, mag_kind, sel, reload_t, inv,
        },
    })
}

// ---------- entrada y dibujo ----------
fn read_input(sel: &mut u8) -> (u8, bool, f32, u8) {
    let mut keys = 0u8;
    if is_key_down(KeyCode::W) { keys |= 1; }
    if is_key_down(KeyCode::S) { keys |= 2; }
    if is_key_down(KeyCode::A) { keys |= 4; }
    if is_key_down(KeyCode::D) { keys |= 8; }
    if is_key_pressed(KeyCode::Key1) { *sel = 0; }
    if is_key_pressed(KeyCode::Key2) { *sel = 1; }
    let mut act = 0u8;
    if is_key_pressed(KeyCode::R) { act |= ACT_RELOAD; }
    if is_key_pressed(KeyCode::E) { act |= ACT_LOOT; }
    if is_key_pressed(KeyCode::Q) { act |= ACT_SWAP; }
    let (mx, my) = mouse_position();
    let aim = (my - screen_height() / 2.0).atan2(mx - screen_width() / 2.0);
    (keys, is_mouse_button_down(MouseButton::Left), aim, act)
}

type Vis = Vec<(f32, f32, f32, f32, f32)>;

fn draw(s: &Snapshot, vis: &mut Vis, host: bool) {
    clear_background(Color::new(0.08, 0.09, 0.1, 1.0));
    if s.views.is_empty() || s.you >= s.views.len() {
        draw_text("Conectando...", 20.0, 40.0, 32.0, WHITE);
        return;
    }
    let v = &s.views[s.you];
    let m = &s.mine;
    let (sw, sh) = (screen_width(), screen_height());
    let ox = sw / 2.0 - v.x;
    let oy = sh / 2.0 - v.y;

    draw_rectangle(ox, oy, W, H, Color::new(0.14, 0.16, 0.14, 1.0));
    draw_rectangle(ox + EXTRACT.0, oy + EXTRACT.1, EXTRACT.2, EXTRACT.3, Color::new(0.1, 0.8, 0.3, 0.35));
    draw_text("EXTRACCION", ox + EXTRACT.0 + 6.0, oy + EXTRACT.1 - 6.0, 20.0, GREEN);
    for &(wx, wy, ww, wh) in WALLS.iter() {
        draw_rectangle(ox + wx, oy + wy, ww, wh, GRAY);
    }
    for c in s.corpses.iter() {
        if !c.items.is_empty() {
            draw_rectangle(ox + c.x - 8.0, oy + c.y - 8.0, 16.0, 16.0, ORANGE);
        }
    }
    for (i, e) in s.views.iter().enumerate() {
        if !e.active || e.extracted { continue; }
        let col = if !e.alive { DARKGRAY } else if i == s.you { SKYBLUE } else if e.bot { RED } else { GREEN };
        draw_circle(ox + e.x, oy + e.y, R, col);
        if e.alive {
            draw_line(ox + e.x, oy + e.y, ox + e.x + e.aim.cos() * 22.0, oy + e.y + e.aim.sin() * 22.0, 3.0, WHITE);
        }
    }
    let dt = get_frame_time();
    for t in vis.iter_mut() {
        draw_line(ox + t.0, oy + t.1, ox + t.2, oy + t.3, 1.5, YELLOW);
        t.4 -= dt;
    }
    vis.retain(|t| t.4 > 0.0);

    // ---- HUD ----
    let wname = match m.wpn { Some(w) => WPNS[(w as usize).min(1)].name, None => "SIN ARMA" };
    let cap = m.wpn.map(|w| WPNS[(w as usize).min(1)].mag).unwrap_or(0);
    draw_text(
        &format!("HP torso: {:.0}  |  Armadura: {:.0}/{:.0}  |  Botin: {}",
                 v.hp, v.armor, ARMOR_DUR, count_item(&m.inv, 4)),
        10.0, 26.0, 26.0, WHITE);
    draw_text(
        &format!("{} {}/{} [{}]  |  Reserva FMJ:{} AP:{}  |  Elegida: {} (1/2)",
                 wname, m.mag, cap, AMMO[(m.mag_kind as usize).min(1)].name,
                 count_item(&m.inv, 0), count_item(&m.inv, 1), AMMO[(m.sel as usize).min(1)].name),
        10.0, 54.0, 24.0, WHITE);
    if m.reload_t > 0.0 {
        draw_text(&format!("RECARGANDO... {:.1}s", m.reload_t), sw / 2.0 - 110.0, sh / 2.0 + 50.0, 30.0, YELLOW);
    } else if m.wpn.is_some() && m.mag == 0 && v.alive {
        draw_text("Sin balas - R para recargar", sw / 2.0 - 140.0, sh / 2.0 + 50.0, 28.0, ORANGE);
    }

    // ---- mochila ----
    let px = sw - 270.0;
    draw_rectangle(px - 10.0, 40.0, 270.0, 30.0 + MAX_SLOTS as f32 * 22.0, Color::new(0.0, 0.0, 0.0, 0.45));
    draw_text(&format!("MOCHILA {}/{}", m.inv.len(), MAX_SLOTS), px, 62.0, 22.0, YELLOW);
    for (k, it) in m.inv.iter().enumerate() {
        draw_text(&format!("{} x{}", item_name(it.kind), it.qty), px, 86.0 + k as f32 * 22.0, 20.0, WHITE);
    }

    // ---- saqueo cercano ----
    if v.alive && !v.extracted {
        let near = s.corpses.iter().find(|c| {
            !c.items.is_empty() && ((c.x - v.x).powi(2) + (c.y - v.y).powi(2)).sqrt() < 50.0
        });
        if let Some(c) = near {
            draw_text("E: saquear todo", 10.0, sh - 150.0 - c.items.len() as f32 * 22.0, 26.0, ORANGE);
            for (k, it) in c.items.iter().enumerate() {
                draw_text(&format!("- {} x{}", item_name(it.kind), it.qty), 10.0,
                          sh - 128.0 - (c.items.len() as f32 - 1.0 - k as f32) * 22.0, 20.0, WHITE);
            }
        }
    }

    if v.extracted {
        draw_text(&format!("EXTRAIDO CON EXITO - botin: {}", count_item(&m.inv, 4)), 10.0, 90.0, 32.0, GREEN);
    } else if !v.alive {
        let msg = if host { "ELIMINADO - F5 reinicia la raid" } else { "ELIMINADO - el host reinicia con F5" };
        draw_text(msg, 10.0, 90.0, 32.0, RED);
    } else if in_extract(v.x, v.y) {
        draw_text("Extrayendo... mantente 3 s", 10.0, 90.0, 28.0, GREEN);
    }
    draw_text("WASD mover | click disparar | R recargar | E saquear | Q cambiar arma | 1/2 municion",
              10.0, sh - 12.0, 20.0, LIGHTGRAY);
}

fn push_tracers(vis: &mut Vis, t: &[(f32, f32, f32, f32)]) {
    for tr in t { vis.push((tr.0, tr.1, tr.2, tr.3, 0.08)); }
}

// ---------- modos ----------
fn handle_packet(w: &mut World, clients: &mut HashMap<SocketAddr, usize>, addr: SocketAddr, b: &[u8]) {
    if b.is_empty() { return; }
    if b[0] == 1 && !clients.contains_key(&addr) {
        if let Some(s) = (1..MAX_PLAYERS).find(|&s| !w.ents[s].active) {
            let mut e = make_ent(s, &mut w.rng);
            e.active = true;
            w.ents[s] = e;
            clients.insert(addr, s);
        }
    } else if b[0] == 2 && b.len() >= 9 {
        if let Some(&s) = clients.get(&addr) {
            let e = &mut w.ents[s];
            e.keys = b[1];
            e.shoot = b[2] != 0;
            e.sel = b[3].min(1);
            e.act = b[4];
            e.aim = f32::from_le_bytes([b[5], b[6], b[7], b[8]]);
        }
    }
}

async fn run_host() {
    let sock = UdpSocket::bind(("0.0.0.0", PORT)).expect("No pude abrir el puerto UDP 7777");
    sock.set_nonblocking(true).unwrap();
    let mut world = World::new();
    let mut clients: HashMap<SocketAddr, usize> = HashMap::new();
    let mut vis: Vis = vec![];
    let mut sel = 0u8;
    let mut buf = [0u8; 512];
    loop {
        while let Ok((n, addr)) = sock.recv_from(&mut buf) {
            handle_packet(&mut world, &mut clients, addr, &buf[..n]);
        }
        let (keys, shoot, aim, act) = read_input(&mut sel);
        {
            let me = &mut world.ents[0];
            me.keys = keys; me.shoot = shoot; me.aim = aim; me.sel = sel; me.act = act;
        }
        if is_key_pressed(KeyCode::F5) { world.reset(); }
        step(&mut world, get_frame_time().min(0.05));
        for (addr, slot) in clients.iter() {
            let _ = sock.send_to(&encode_state(&world, *slot as u8), addr);
        }
        let snap = decode_state(&encode_state(&world, 0)).unwrap_or_default();
        push_tracers(&mut vis, &world.tracers);
        world.tracers.clear();
        draw(&snap, &mut vis, true);
        let info = format!("HOST puerto {} | jugadores: {}", PORT, clients.len() + 1);
        draw_text(&info, 10.0, 120.0, 22.0, YELLOW);
        next_frame().await;
    }
}

async fn run_client(mut addr: String) {
    if !addr.contains(':') { addr = format!("{}:{}", addr, PORT); }
    let sock = UdpSocket::bind("0.0.0.0:0").expect("No pude abrir socket");
    sock.connect(&addr).expect("Direccion invalida");
    sock.set_nonblocking(true).unwrap();
    let mut snap = Snapshot::default();
    let mut vis: Vis = vec![];
    let mut sel = 0u8;
    let mut act_bits = 0u8;
    let mut act_t = 0.0f32;
    let mut join_t = 0.0f32;
    let mut buf = [0u8; 4096];
    loop {
        let dt = get_frame_time();
        join_t -= dt;
        if snap.views.is_empty() && join_t <= 0.0 {
            let _ = sock.send(&[1]);
            join_t = 0.5;
        }
        while let Ok(n) = sock.recv(&mut buf) {
            if let Some(s) = decode_state(&buf[..n]) {
                push_tracers(&mut vis, &s.tracers);
                snap = s;
            }
        }
        let (keys, shoot, aim, pressed) = read_input(&mut sel);
        if pressed != 0 { act_bits |= pressed; act_t = 0.2; }
        act_t -= dt;
        if act_t <= 0.0 { act_bits = 0; }
        let mut p = vec![2u8, keys, shoot as u8, sel, act_bits];
        p.extend_from_slice(&aim.to_le_bytes());
        let _ = sock.send(&p);
        draw(&snap, &mut vis, false);
        next_frame().await;
    }
}

#[macroquad::main("Breakout2D MVP")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s == "join").unwrap_or(false) {
        run_client(args.get(2).cloned().unwrap_or_else(|| "127.0.0.1".to_string())).await;
    } else {
        run_host().await;
    }
}
