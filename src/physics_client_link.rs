//! RUSTUDIO_SOLVER=studio: the player's character and the client's own loose parts
//! (round HANG, solver/JOINTS-NOTES.md).
//!
//! A LocalScript that clones a part, destroys the original and unanchors the clone
//! makes a part only the client has (FtF's ClientObjectScripts: the JToH rope
//! platforms, the spawn lanterns). Studio's client simulates it in the same world
//! as its character: the character stands on it, pushes it and is pushed back, and
//! the original it destroyed is gone from that world.
//!
//! rustudio steps the character in the server's world and those parts in the
//! client's (`client::ClientSim`). Before, the two never met: the character stood on
//! the server's original (still there, anchored), the clone hung beside it untouched
//! (a board on a rope shorter than its gap rose 1.01 stud and the character's feet
//! were inside it; a lantern walked into did not move).
//!
//! Here, once a frame:
//! * every client body near the character gets a stand-in in the server's world: a
//!   dynamic body with its shapes and its mass, at its pose and velocity, without
//!   gravity, touching nothing but the player's character (`Link::filter`);
//! * the world steps: the character stands on the stand-in (floor rays, the floor
//!   reaction), walks into it and is held up by it, as by the part itself;
//! * what the frame did to the stand-in's velocity (the character's floor reaction
//!   and contacts; it has no other force) goes to the client body before the
//!   client's world steps (`ClientSim::kick`), which adds its ropes and gravity;
//! * the character's own parts stay out of the client's world (they would push the
//!   body a second time);
//! * a replicated part the client took out of its Workspace (destroyed, unparented)
//!   no longer holds up or stops the player's character (`hidden`): floor rays and
//!   contacts skip it. The server's own bodies still meet it.
//!
//! The rapier path keeps its old behaviour (none of this runs there).
use super::*;
use rapier3d::geometry::BoundingVolume;

/// Studs around the character within which a client body gets a stand-in, plus this
/// many seconds of the two speeds.
const NEAR_MARGIN: f32 = 4.0;

fn debug() -> bool {
    static D: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *D.get_or_init(|| std::env::var_os("RUSTUDIO_LINK_DEBUG").is_some())
}

/// RUSTUDIO_NO_CLIENT_LINK=1 or the rustudio option: off (A/B and perf
/// checks; the studio path as before).
pub fn off() -> bool {
    !crate::rustudio_settings::client_link()
}
const NEAR_LEAD_S: f32 = 0.25;

struct Proxy {
    body: rp::RigidBodyHandle,
    colliders: Vec<rp::ColliderHandle>,
    parts: Vec<Ref>,
    /// The velocities it was given at the frame's start.
    v0: Vec3,
    w0: Vec3,
    /// What the client's world did to the body in its last step (gravity, ropes,
    /// movers) as a force and torque, put on the stand-in every tick so the character
    /// meets a body held as the real one is; and the velocity change they made this
    /// frame (kept out of the kick).
    force: Vec3,
    torque: Vec3,
    pred_v: Vec3,
    pred_w: Vec3,
}

#[derive(Default)]
pub struct Link {
    /// Client body (its handle in the client's world) → its stand-in here.
    proxies: HashMap<rp::RigidBodyHandle, Proxy>,
    /// Client part → stand-in body (`Physics::body_of`).
    part_body: HashMap<Ref, rp::RigidBodyHandle>,
    proxy_colliders: HashSet<rp::ColliderHandle>,
    /// Client part → (friction, friction weight) of its shape (the floor's friction).
    friction: HashMap<Ref, (f32, f32)>,
    /// Server parts the client took out of its Workspace → the client instance taken out
    /// (the part's own copy, or an ancestor's).
    hidden: HashMap<Ref, Ref>,
    /// The player's character body (the only thing a stand-in touches).
    pub(super) player: Option<rp::RigidBodyHandle>,
}

impl Link {
    pub fn is_empty(&self) -> bool {
        self.proxies.is_empty() && self.hidden.is_empty()
    }

    pub(super) fn body_of(&self, r: Ref) -> Option<rp::RigidBodyHandle> {
        if self.part_body.is_empty() {
            return None;
        }
        self.part_body.get(&r).copied()
    }

    pub(super) fn friction_of(&self, r: Ref) -> Option<(f32, f32)> {
        self.friction.get(&r).copied()
    }

    /// Whether a query by the player's character should skip this collider.
    pub(super) fn hides(&self, h: rp::ColliderHandle, by_collider: &rustc_hash::FxHashMap<rp::ColliderHandle, Ref>) -> bool {
        !self.hidden.is_empty() && by_collider.get(&h).is_some_and(|r| self.hidden.contains_key(r))
    }

    /// The pair rule (`OwnerHooks`): Some(answer) for the pairs this decides.
    pub(super) fn filter(&self, c: &rp::PairFilterContext, by_collider: &rustc_hash::FxHashMap<rp::ColliderHandle, Ref>) -> Option<Option<rp::SolverFlags>> {
        if self.is_empty() {
            return None;
        }
        let me = |b: Option<rp::RigidBodyHandle>| b.is_some() && b == self.player;
        let (p1, p2) = (self.proxy_colliders.contains(&c.collider1), self.proxy_colliders.contains(&c.collider2));
        if p1 || p2 {
            let ok = !(p1 && p2) && if p1 { me(c.rigid_body2) } else { me(c.rigid_body1) };
            return Some(ok.then_some(rp::SolverFlags::COMPUTE_RIGID_IMPULSES));
        }
        if !self.hidden.is_empty() && (me(c.rigid_body1) || me(c.rigid_body2)) {
            let other = if me(c.rigid_body1) { c.collider2 } else { c.collider1 };
            if by_collider.get(&other).is_some_and(|r| self.hidden.contains_key(r)) {
                return Some(None);
            }
        }
        None
    }
}

impl Physics {
    /// The replicated parts under `trees` (client refs whose parent changed, or every
    /// replicated part) that the client has out of its Workspace now: hidden from the
    /// player's character; back in, shown again. (Studio path only.)
    pub(super) fn link_note_trees(&mut self, server: &Document, client: &Document, c2s: &std::collections::HashMap<Ref, Ref>, trees: &[Ref], all: bool) {
        let ws = client.workspace();
        let inside = |c: Ref| ws.is_some_and(|w| client.dom.get_by_ref(c).is_some() && client.is_ancestor(w, c));
        if all {
            // (The client's start-up flood: every replicated part once.)
            let mut hidden = HashMap::default();
            for (c, s) in c2s {
                if self.parts.contains_key(s) && !inside(*c) {
                    hidden.insert(*s, *c);
                }
            }
            self.link.hidden = hidden;
            return;
        }
        // Back in (or the server dropped it): shown again. (Few parts.)
        if !self.link.hidden.is_empty() {
            let parts = &self.parts;
            self.link.hidden.retain(|s, c| parts.contains_key(s) && !inside(*c));
        }
        // Taken out: everything under it. (Most tree marks are arrivals, which are in.)
        for c in trees {
            let Some(&s) = c2s.get(c) else { continue };
            if inside(*c) {
                continue;
            }
            for d in server.subtree(s) {
                if self.parts.contains_key(&d) {
                    self.link.hidden.insert(d, *c);
                }
            }
        }
    }

    /// Once a frame before the character's step (studio path): the stand-ins of the
    /// client's own bodies near the player's character, where and how fast those are.
    pub fn link_client_bodies(&mut self, cs: &client::ClientSim, c2s: &std::collections::HashMap<Ref, Ref>, player_model: Option<Ref>) {
        if self.studio.is_none() || off() {
            return;
        }
        let player = player_model.and_then(|m| self.hums.by_model.get(&m)).map(|h| h.body);
        self.link.player = player;
        let mut want: Vec<rp::RigidBodyHandle> = Vec::new();
        let region = player.and_then(|b| self.world.bodies.get(b)).and_then(|b| {
            let mut bb: Option<rp::Aabb> = None;
            for ch in b.colliders() {
                let Some(c) = self.world.colliders.get(*ch) else { continue };
                let x = c.compute_aabb();
                bb = Some(match bb {
                    Some(mut m) => {
                        m.merge(&x);
                        m
                    }
                    None => x,
                });
            }
            bb.map(|bb| (bb, gv(b.linvel()).length()))
        });
        if let (Some((pbb, pspeed)), Some(cp)) = (region, cs.physics()) {
            let mut asms: Vec<&Assembly> = cp.asms.values().collect();
            asms.sort_unstable_by_key(|a| a.body.into_raw_parts());
            for a in asms {
                let Some(b) = cp.world.bodies.get(a.body) else { continue };
                if !b.is_dynamic() || a.parts.iter().any(|r| cp.external.contains(r) || c2s.contains_key(r)) {
                    continue;
                }
                let mut bb: Option<rp::Aabb> = None;
                for r in &a.parts {
                    let Some(c) = cp.parts.get(r).and_then(|pb| cp.world.colliders.get(pb.collider)) else { continue };
                    if c.is_sensor() {
                        continue;
                    }
                    let x = c.compute_aabb();
                    bb = Some(match bb {
                        Some(mut m) => {
                            m.merge(&x);
                            m
                        }
                        None => x,
                    });
                }
                let Some(bb) = bb else { continue };
                let lead = (pspeed + gv(b.linvel()).length()) * NEAR_LEAD_S;
                if pbb.loosened(NEAR_MARGIN + lead).intersects(&bb) {
                    want.push(a.body);
                }
            }
        }
        if debug() && self.steps % 240 == 0 {
            eprintln!("LINKSTATE step={} player={:?} proxies={} hidden={} want={} client_asms={}", self.steps, player, self.link.proxies.len(), self.link.hidden.len(), want.len(), cs.physics().map_or(0, |p| p.asms.len()));
        }
        // Stand-ins no longer wanted.
        let gone: Vec<rp::RigidBodyHandle> = self.link.proxies.keys().filter(|k| !want.contains(k)).copied().collect();
        for k in gone {
            self.drop_proxy(k);
        }
        let Some(cp) = cs.physics() else { return };
        for cb in want {
            let Some(b) = cp.world.bodies.get(cb) else { continue };
            let (pose, v, w) = (*b.position(), gv(b.linvel()), gv(b.angvel()));
            if !self.link.proxies.contains_key(&cb) {
                self.make_proxy(cp, cb);
            }
            let Some(p) = self.link.proxies.get_mut(&cb) else { continue };
            p.v0 = v;
            p.w0 = w;
            p.pred_v = Vec3::ZERO;
            p.pred_w = Vec3::ZERO;
            let body = p.body;
            let cols = p.colliders.clone();
            // (Solved on every tick, as the character it touches.)
            if let Some(sv) = self.studio.as_mut() {
                sv.set_level_request(body, 3, 1);
            }
            if let Some(pb) = self.world.bodies.get_mut(body) {
                pb.set_position(pose, true);
                pb.set_linvel(rv(v), true);
                pb.set_angvel(rv(w), true);
            }
            // (Queries before the next tick see the shapes where the body is.)
            for ch in cols {
                if let Some(c) = self.world.colliders.get_mut(ch) {
                    let local = c.position_wrt_parent().copied().unwrap_or(rp::Pose::IDENTITY);
                    c.set_position(pose * local);
                }
            }
        }
        // The player's colliders go through the pair rule while there is something
        // for it to decide (rapier asks only for flagged colliders).
        if !self.link.is_empty() {
            if let Some(b) = player.and_then(|b| self.world.bodies.get(b)) {
                let list: Vec<rp::ColliderHandle> = b.colliders().to_vec();
                for ch in list {
                    if let Some(c) = self.world.colliders.get_mut(ch) {
                        if !c.active_hooks().contains(rp::ActiveHooks::FILTER_CONTACT_PAIRS) {
                            c.set_active_hooks(c.active_hooks() | rp::ActiveHooks::FILTER_CONTACT_PAIRS);
                        }
                    }
                }
            }
        }
    }

    fn make_proxy(&mut self, cp: &Physics, cb: rp::RigidBodyHandle) {
        let Some(b) = cp.world.bodies.get(cb) else { return };
        let Some(a) = cp.asms.values().find(|a| a.body == cb) else { return };
        let mprops = b.mass_properties().local_mprops;
        let rb = rp::RigidBodyBuilder::dynamic()
            .pose(*b.position())
            .gravity_scale(0.0)
            .can_sleep(false)
            .additional_mass_properties(mprops);
        let body = self.world.insert_body(rb.build());
        let mut colliders = Vec::new();
        let mut parts = Vec::new();
        for r in &a.parts {
            let Some(c) = cp.parts.get(r).and_then(|pb| cp.world.colliders.get(pb.collider)) else { continue };
            parts.push(*r);
            if c.is_sensor() {
                continue;
            }
            let copy = rp::ColliderBuilder::new(c.shared_shape().clone())
                .position(c.position_wrt_parent().copied().unwrap_or(rp::Pose::IDENTITY))
                .friction(c.friction())
                .restitution(c.restitution())
                .friction_combine_rule(c.friction_combine_rule())
                .restitution_combine_rule(c.restitution_combine_rule())
                .collision_groups(c.collision_groups())
                .user_data(c.user_data)
                .density(0.0)
                .active_hooks(rp::ActiveHooks::FILTER_CONTACT_PAIRS);
            let gc = self.world.insert_collider(copy, Some(body));
            if let Some(sv) = self.studio.as_mut() {
                sv.note_collider(gc, *r, c.shape());
            }
            self.by_collider.insert(gc, *r);
            self.link.proxy_colliders.insert(gc);
            colliders.push(gc);
        }
        // The client world's records of the body (its mass record, its box).
        if let (Some(sv), Some(csv)) = (self.studio.as_mut(), cp.studio.as_ref()) {
            let (m, bx, len) = csv.body_notes(cb);
            sv.note_mass(body, m);
            if let Some((lo, hi)) = bx {
                sv.note_box(body, lo, hi);
            }
            if let Some(l) = len {
                sv.note_char_len(body, l);
            }
        }
        for r in &parts {
            self.link.part_body.insert(*r, body);
            if let Some(c) = cp.parts.get(r).and_then(|pb| cp.world.colliders.get(pb.collider)) {
                self.link.friction.insert(*r, (c.friction(), hum_body::weights_of(c).0));
            }
        }
        self.dyn_version += 1;
        self.link.proxies.insert(cb, Proxy { body, colliders, parts, v0: Vec3::ZERO, w0: Vec3::ZERO, force: Vec3::ZERO, torque: Vec3::ZERO, pred_v: Vec3::ZERO, pred_w: Vec3::ZERO });
    }

    fn drop_proxy(&mut self, cb: rp::RigidBodyHandle) {
        let Some(p) = self.link.proxies.remove(&cb) else { return };
        for c in &p.colliders {
            self.by_collider.remove(c);
            self.link.proxy_colliders.remove(c);
        }
        for r in &p.parts {
            self.link.part_body.remove(r);
            self.link.friction.remove(r);
        }
        if let Some(sv) = self.studio.as_mut() {
            sv.note_mass(p.body, None);
        }
        self.world.remove_body(p.body);
        self.dyn_version += 1;
    }

    /// After the frame's world steps: what they did to each stand-in's velocities
    /// (client body, Δv, Δω), for `ClientSim::kick`.
    pub fn link_kicks(&self) -> Vec<(rp::RigidBodyHandle, Vec3, Vec3)> {
        let mut out = Vec::new();
        for (cb, p) in &self.link.proxies {
            let Some(b) = self.world.bodies.get(p.body) else { continue };
            let dv = gv(b.linvel()) - p.v0 - p.pred_v;
            let dw = gv(b.angvel()) - p.w0 - p.pred_w;
            if debug() {
                let lvl = self.studio.as_ref().map(|sv| sv.next_solve(p.body));
                eprintln!("LINKKICK step={} lvl={lvl:?} v0={:?} dv={dv:?} dw={dw:?} pos={:?}", self.steps, p.v0, b.translation());
            }
            if dv.length_squared() > 1e-12 || dw.length_squared() > 1e-12 {
                out.push((*cb, dv, dw));
            }
        }
        out.sort_unstable_by_key(|(b, _, _)| b.into_raw_parts());
        out
    }

    /// Before each world tick (after the movers cleared last tick's forces): the
    /// client world's own push on each stand-in.
    pub(super) fn link_forces(&mut self, h: f32) {
        if self.link.proxies.is_empty() {
            return;
        }
        for p in self.link.proxies.values_mut() {
            if p.force == Vec3::ZERO && p.torque == Vec3::ZERO {
                continue;
            }
            let Some(b) = self.world.bodies.get_mut(p.body) else { continue };
            b.add_force(rv(p.force), true);
            b.add_torque(rv(p.torque), true);
            let m = b.mass();
            if m > 0.0 {
                p.pred_v += p.force / m * h;
            }
            p.pred_w += hum_body::world_inv_inertia(b) * p.torque * h;
            self.pushed.insert(p.body);
        }
    }

    /// After the client's world stepped `dt`: what its step did to each linked body
    /// (`ClientSim::link_motion`), as the force and torque for the next frame.
    pub fn link_note_motion(&mut self, motion: &[(rp::RigidBodyHandle, Vec3, Vec3)], dt: f32) {
        if dt <= 0.0 {
            return;
        }
        for (cb, dv, dw) in motion {
            let Some(p) = self.link.proxies.get_mut(cb) else { continue };
            let Some(b) = self.world.bodies.get(p.body) else { continue };
            p.force = *dv * b.mass() / dt;
            let inv = hum_body::world_inv_inertia(b);
            let i = if inv.determinant().abs() > 1e-12 { inv.inverse() } else { Mat3::ZERO };
            p.torque = i * *dw / dt;
        }
    }

    /// Whether this world has stand-ins of client bodies.
    pub fn link_active(&self) -> bool {
        !self.link.proxies.is_empty()
    }

    /// The client bodies with stand-ins.
    pub fn link_bodies(&self) -> Vec<rp::RigidBodyHandle> {
        let mut v: Vec<_> = self.link.proxies.keys().copied().collect();
        v.sort_unstable_by_key(|b| b.into_raw_parts());
        v
    }
}

impl client::ClientSim {
    /// The character's push on the client's own bodies (`Physics::link_kicks`);
    /// `linked`: every body with a stand-in (their velocities after the kick are
    /// the start of `link_motion`).
    pub fn kick(&mut self, kicks: &[(rp::RigidBodyHandle, Vec3, Vec3)], linked: &[rp::RigidBodyHandle]) {
        self.link_start.clear();
        let Some(p) = self.physics.as_mut() else { return };
        for (cb, dv, dw) in kicks {
            if let Some(b) = p.world.bodies.get_mut(*cb) {
                if b.is_dynamic() {
                    let v = gv(b.linvel()) + *dv;
                    let w = gv(b.angvel()) + *dw;
                    b.set_linvel(rv(v), true);
                    b.set_angvel(rv(w), true);
                }
            }
        }
        for cb in linked {
            if let Some(b) = p.world.bodies.get(*cb) {
                self.link_start.insert(*cb, (gv(b.linvel()), gv(b.angvel())));
            }
        }
    }

    /// After its step: what the step did to each linked body's velocities.
    pub fn link_motion(&self) -> Vec<(rp::RigidBodyHandle, Vec3, Vec3)> {
        let Some(p) = self.physics.as_ref() else { return Vec::new() };
        let mut out: Vec<_> = self
            .link_start
            .iter()
            .filter_map(|(cb, (v, w))| p.world.bodies.get(*cb).map(|b| (*cb, gv(b.linvel()) - *v, gv(b.angvel()) - *w)))
            .collect();
        out.sort_unstable_by_key(|(b, _, _)| b.into_raw_parts());
        out
    }
}
