//! A Tool going into a character's hand turns its Handle's CanCollide off, as
//! Studio's grip builder does (see `gripped_handles`). The write itself is in
//! runtime.rs's parent change, after the hierarchy signals.

use rbx_dom_weak::types::{Ref, Variant};
use rustudio_core::doc::{self, Document};

/// The Handles of the Tools that this parent change puts in a character's grip, whose
/// CanCollide goes false. Studio: a Tool's equip steps end in the grip
/// builder, which needs the Tool's parent to be
/// a Model with a Humanoid (the first Humanoid child), that Humanoid's
/// right hand (RightHand for R15, Right Arm for R6) and the Tool's Handle
/// (the child BasePart named "Handle"); it checks or remakes the RightGrip
/// weld and calls the Handle's CanCollide setter with false. Only the Handle: other tool parts keep theirs. Nothing puts it
/// back on unequip. Measured in Studio Play: the Tool parented into
/// the character or a Model carrying a held Tool entering the Workspace sets it, on the
/// server and on the client; a dead Humanoid's model too; not outside the Workspace,
/// not under a Folder, not without the right arm, not for a Handle added later.
pub fn gripped_handles(d: &Document, r: Ref, enters_ws: bool) -> Vec<Ref> {
    let Some(ws) = d.workspace() else { return Vec::new() };
    let mut tools: Vec<Ref> = Vec::new();
    if d.class_of(r).is_some_and(|c| doc::is_a(&c, "Tool")) {
        tools.push(r);
    }
    if enters_ws {
        tools.extend(d.dom.descendants_of(r).filter(|i| doc::is_a(&i.class, "Tool")).map(|i| i.referent()));
    }
    let child = |p: Ref, name: &str| {
        d.dom.get_by_ref(p)?.children().iter().copied().find(|c| d.dom.get_by_ref(*c).is_some_and(|i| i.name == name && doc::is_a(&i.class, "BasePart")))
    };
    tools
        .into_iter()
        .filter_map(|t| {
            let model = d.dom.get_by_ref(t)?.parent();
            if !d.is_ancestor(ws, t) || !d.class_of(model).is_some_and(|c| doc::is_a(&c, "Model")) {
                return None;
            }
            let hum = d.dom.get_by_ref(model)?.children().iter().copied().find(|c| d.class_of(*c).is_some_and(|k| doc::is_a(&k, "Humanoid")))?;
            let r15 = rustudio_render::scene::enum_u32(d.prop(hum, "RigType")).unwrap_or(0) != 0;
            child(model, if r15 { "RightHand" } else { "Right Arm" })?;
            let handle = child(t, "Handle")?;
            matches!(d.prop(handle, "CanCollide"), Some(Variant::Bool(true)) | None).then_some(handle)
        })
        .collect()
}

/// Play / Run start: loading the session's DataModel parents every Tool, so each Tool the
/// place saved in a character-like Model in the Workspace (an NPC's sword) is gripped and
/// its Handle's CanCollide is false before any script runs (the same
/// ancestry path). Returns the Handles turned off.
pub fn grip_at_load(d: &mut Document) -> Vec<Ref> {
    let Some(ws) = d.workspace() else { return Vec::new() };
    let tools: Vec<Ref> = d.dom.descendants_of(ws).filter(|i| doc::is_a(&i.class, "Tool")).map(|i| i.referent()).collect();
    let handles: Vec<Ref> = tools.into_iter().flat_map(|t| gripped_handles(d, t, false)).collect();
    for h in &handles {
        if let Some(i) = d.dom.get_by_ref_mut(*h) {
            i.properties.insert(rbx_dom_weak::ustr("CanCollide"), Variant::Bool(false));
        }
    }
    handles
}

#[cfg(test)]
mod tests {
    use crate::runtime::{RunMode, Runtime, World};
    use rbx_dom_weak::InstanceBuilder;
    use rustudio_core::doc::Document;

    /// The measured Studio Play cases, run as a server Script.
    fn run(source: &str, mode: RunMode) -> Vec<String> {
        let mut d = Document::new_place();
        let parent = d.find_child(d.dom.root_ref(), "ServerScriptService").unwrap();
        let s = d.dom.insert(parent, InstanceBuilder::new("Script").with_name("ToolGrip").with_property("Source", source.to_string()));
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, mode));
        rt.bind_globals().unwrap();
        rt.run_script(s);
        for _ in 0..3 {
            rt.step(1.0 / 60.0);
        }
        rt.leave().output.into_iter().map(|(_, s)| s).collect()
    }

    const RIG: &str = r#"
local function rig(r15, parent)
  local m = Instance.new("Model")
  local h = Instance.new("Humanoid"); h.RigType = r15 and Enum.HumanoidRigType.R15 or Enum.HumanoidRigType.R6; h.Parent = m
  local arm = Instance.new("Part"); arm.Name = r15 and "RightHand" or "Right Arm"; arm.Anchored = true; arm.Parent = m
  m.Parent = parent
  return m
end
local function sword()
  local t = Instance.new("Tool")
  local h = Instance.new("Part"); h.Name = "Handle"; h.Size = Vector3.new(1, 0.8, 4); h.Parent = t
  local b = Instance.new("Part"); b.Name = "Blade"; b.Parent = t
  return t, h, b
end
"#;

    #[test]
    fn equip_turns_only_the_handle_off() {
        let out = run(
            &format!(
                "{RIG}{}",
                r#"
local m = rig(true, workspace)
local t, h, b = sword(); t.Parent = game.ServerStorage
local seen
t.AncestryChanged:Connect(function() if t.Parent == m then seen = h.CanCollide end end)
local changed = 0
h:GetPropertyChangedSignal("CanCollide"):Connect(function() changed += 1 end)
t.Parent = m
print("R15", h.CanCollide, b.CanCollide, seen, changed)
t.Parent = game.ServerStorage
print("unequipped", h.CanCollide)
h.CanCollide = true; t.Parent = m; h.CanCollide = true
print("script set true while held", h.CanCollide)
local m6 = rig(false, workspace); local t6, h6 = sword(); t6.Parent = m6
print("R6", h6.CanCollide)
"#
            ),
            RunMode::Play,
        );
        let o = out.join("
");
        assert!(o.contains("R15 false true true 1"), "{o}");
        assert!(o.contains("unequipped false"), "{o}");
        assert!(o.contains("script set true while held true"), "{o}");
        assert!(o.contains("R6 false"), "{o}");
    }

    #[test]
    fn saved_npc_tools_are_gripped_at_load() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let npc = d.dom.insert(ws, InstanceBuilder::new("Model").with_name("Bandit"));
        d.dom.insert(npc, InstanceBuilder::new("Humanoid"));
        d.dom.insert(npc, InstanceBuilder::new("Part").with_name("Right Arm"));
        let tool = d.dom.insert(npc, InstanceBuilder::new("Tool").with_name("Steel Sword"));
        let handle = d.dom.insert(tool, InstanceBuilder::new("Part").with_name("Handle").with_property("CanCollide", true));
        let blade = d.dom.insert(tool, InstanceBuilder::new("Part").with_name("Blade").with_property("CanCollide", true));
        let ss = d.find_child(d.dom.root_ref(), "ServerStorage").unwrap();
        let stored = d.dom.insert(ss, InstanceBuilder::new("Tool"));
        let stored_h = d.dom.insert(stored, InstanceBuilder::new("Part").with_name("Handle").with_property("CanCollide", true));
        assert_eq!(super::grip_at_load(&mut d), vec![handle]);
        assert_eq!(d.prop(handle, "CanCollide"), Some(rbx_dom_weak::types::Variant::Bool(false)));
        assert_eq!(d.prop(blade, "CanCollide"), Some(rbx_dom_weak::types::Variant::Bool(true)));
        assert_eq!(d.prop(stored_h, "CanCollide"), Some(rbx_dom_weak::types::Variant::Bool(true)));
    }

    #[test]
    fn only_a_handed_humanoid_model_in_the_workspace() {
        let out = run(
            &format!(
                "{RIG}{}",
                r#"
local rs = game:GetService("ReplicatedStorage")
local m = rig(true, rs); local t, h = sword(); t.Parent = m
print("outside workspace", h.CanCollide)
m.Parent = workspace
print("model enters workspace", h.CanCollide)
local plain = Instance.new("Model"); plain.Parent = workspace
local t2, h2 = sword(); t2.Parent = plain
print("no humanoid", h2.CanCollide)
local f = Instance.new("Folder"); Instance.new("Humanoid", f); local a = Instance.new("Part"); a.Name = "Right Arm"; a.Parent = f; f.Parent = workspace
local t3, h3 = sword(); t3.Parent = f
print("folder", h3.CanCollide)
local noarm = Instance.new("Model"); Instance.new("Humanoid", noarm); noarm.Parent = workspace
local t4, h4 = sword(); t4.Parent = noarm
print("no arm", h4.CanCollide)
local acc = Instance.new("Accessory"); local ah = Instance.new("Part"); ah.Name = "Handle"; ah.Parent = acc
acc.Parent = rig(true, workspace)
print("accessory", ah.CanCollide)
"#
            ),
            RunMode::Play,
        );
        let o = out.join("
");
        for want in ["outside workspace true", "model enters workspace false", "no humanoid true", "folder true", "no arm true", "accessory true"] {
            assert!(o.contains(want), "{want}: {o}");
        }
    }
}
