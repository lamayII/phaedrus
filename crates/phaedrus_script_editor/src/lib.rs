//! Phaedrus script editor: Monaco (via the `monaco` crate) with Luau
//! highlighting and Roblox-aware intellisense, compiled to WebAssembly and
//! hosted by the editor in a native web view.
//!
//! The analysis (`analysis`, `features`) is plain Rust and is unit tested
//! natively; only `web` touches the browser.

pub mod analysis;
pub mod api;
pub mod features;
pub mod settings;
pub mod syntax;
pub mod tree;
pub mod types;

#[cfg(target_arch = "wasm32")]
mod web;

#[cfg(test)]
mod tests {
    use crate::analysis::{Ctx, ModuleCache};
    use crate::features::{Completion, complete, hover, signature_help};
    use crate::tree::Tree;

    const TREE: &str = r#"
    ["root", "Game", "DataModel", [
        ["ws", "Workspace", "Workspace", [
            ["part", "Baseplate", "Part", []],
            ["model", "House", "Model", [
                ["door", "Door", "Part", []],
                ["sp", "Front Step", "Part", []]
            ]]
        ]],
        ["plr", "Players", "Players", []],
        ["rs", "ReplicatedStorage", "ReplicatedStorage", [
            ["mod", "Util", "ModuleScript", [], "local Util = {}\nUtil.Version = 2\nfunction Util.greet(name: string): string\n\treturn 'hi ' .. name\nend\nfunction Util.spawnPart(): Part\n\treturn Instance.new('Part')\nend\nreturn Util\n"],
            ["cls", "Gun", "ModuleScript", [], "local Gun = {}\nGun.__index = Gun\nfunction Gun.new(ammo: number)\n\tlocal self = setmetatable({}, Gun)\n\tself.Ammo = ammo\n\treturn self\nend\nfunction Gun:Fire()\n\tself.Ammo -= 1\nend\nreturn Gun\n"]
        ]],
        ["sss", "ServerScriptService", "ServerScriptService", [
            ["script", "Main", "Script", [
                ["cfg", "Config", "Configuration", []]
            ]]
        ]]
    ]]"#;

    fn run<R>(f: impl FnOnce(&Ctx) -> R) -> R {
        let tree = Tree::from_json(TREE);
        let modules = ModuleCache::default();
        let script = tree.by_id.get("script").copied();
        let ctx = Ctx { tree: &tree, script, modules: &modules, depth: 0 };
        f(&ctx)
    }

    /// Completion labels with the cursor at `|`.
    fn labels(src: &str) -> Vec<String> {
        let offset = src.find('|').expect("cursor");
        let text = src.replace('|', "");
        run(|ctx| complete(&text, offset, ctx)).into_iter().map(|c: Completion| c.label).collect()
    }

    fn hover_at(src: &str) -> Option<String> {
        let offset = src.find('|').expect("cursor");
        let text = src.replace('|', "");
        run(|ctx| hover(&text, offset, ctx))
    }

    #[track_caller]
    fn has(labels: &[String], want: &[&str]) {
        for w in want {
            assert!(labels.iter().any(|l| l == w), "missing {w:?} in {labels:?}");
        }
    }

    #[track_caller]
    fn lacks(labels: &[String], unwanted: &[&str]) {
        for w in unwanted {
            assert!(!labels.iter().any(|l| l == w), "unexpected {w:?}");
        }
    }

    #[test]
    fn instance_members_and_children() {
        let l = labels("local p = workspace.Baseplate\np.|");
        has(&l, &["Anchored", "Position", "Touched", "Name", "Parent"]);
        lacks(&l, &["Destroy"]);
        let l = labels("workspace.House.|");
        has(&l, &["Door", "Front Step", "PrimaryPart"]);
        let l = labels("local p = workspace.Baseplate\np:|");
        has(&l, &["Destroy", "FindFirstChild", "ApplyImpulse", "GetPivot"]);
        lacks(&l, &["Anchored"]);
    }

    #[test]
    fn services_and_class_specific_members() {
        let l = labels("local Players = game:GetService(\"Players\")\nPlayers.|");
        has(&l, &["LocalPlayer", "PlayerAdded", "MaxPlayers"]);
        let l = labels("local plr = game.Players.LocalPlayer\nplr.|");
        has(&l, &["Character", "UserId", "CharacterAdded"]);
        let l = labels("local h = Instance.new(\"Humanoid\")\nh.|");
        has(&l, &["Health", "WalkSpeed", "Died"]);
        let l = labels("game:GetService(\"|");
        has(&l, &["Players", "ReplicatedStorage", "RunService", "TweenService"]);
        let l = labels("Instance.new(\"|");
        has(&l, &["Part", "Model", "Folder"]);
        lacks(&l, &["Workspace"]);
    }

    #[test]
    fn script_relative_paths() {
        let l = labels("script.|");
        has(&l, &["Config", "Enabled", "Parent"]);
        let l = labels("script.Parent.|");
        has(&l, &["Main"]);
        let l = labels("script:FindFirstChild(\"|");
        has(&l, &["Config"]);
        let l = labels("local cfg = script:WaitForChild(\"Config\")\ncfg.|");
        has(&l, &["Name"]);
    }

    #[test]
    fn module_scripts() {
        let l = labels("local Util = require(game.ReplicatedStorage.Util)\nUtil.|");
        has(&l, &["Version", "greet", "spawnPart"]);
        let l = labels("local Util = require(game:GetService(\"ReplicatedStorage\"):WaitForChild(\"Util\"))\nlocal p = Util.spawnPart()\np.|");
        has(&l, &["Anchored"]);
        let l = labels("local Gun = require(game.ReplicatedStorage.Gun)\nlocal g = Gun.new(5)\ng:|");
        has(&l, &["Fire"]);
        let l = labels("local Gun = require(game.ReplicatedStorage.Gun)\nlocal g = Gun.new(5)\ng.|");
        has(&l, &["Ammo"]);
    }

    #[test]
    fn local_variables_and_types() {
        let l = labels("local count = 5\nlocal name = \"x\"\nlocal function helper(a, b) return a end\n|");
        has(&l, &["count", "name", "helper", "print", "game", "local"]);
        let l = labels("do\n\tlocal inner = 1\nend\n|");
        lacks(&l, &["inner"]);
        let l = labels("local v = Vector3.new(1, 2, 3)\nv.|");
        has(&l, &["X", "Magnitude", "Unit"]);
        let l = labels("local v = Vector3.new(1, 2, 3) * 2\nv:|");
        has(&l, &["Dot", "Cross", "Lerp"]);
        let l = labels("local cf = CFrame.new() * CFrame.Angles(0, 1, 0)\ncf.|");
        has(&l, &["LookVector", "Position"]);
        let l = labels("local s = \"abc\"\ns:|");
        has(&l, &["upper", "split", "format"]);
        let l = labels("local t = {alpha = 1, beta = Vector3.zero}\nt.|");
        has(&l, &["alpha", "beta"]);
        let l = labels("local part: BasePart = nil\npart.|");
        has(&l, &["Anchored"]);
        let l = labels("local m = (workspace.House :: Model)\nm:|");
        has(&l, &["GetPivot", "PivotTo"]);
    }

    #[test]
    fn callbacks_get_signal_parameter_types() {
        let l = labels("workspace.Baseplate.Touched:Connect(function(hit)\n\thit.|\nend)");
        has(&l, &["Anchored", "Parent"]);
        let l = labels("game.Players.PlayerAdded:Connect(function(player)\n\tplayer.|\nend)");
        has(&l, &["UserId", "Character"]);
        let l = labels("workspace.Baseplate.Touched:|");
        has(&l, &["Connect", "Once", "Wait"]);
    }

    #[test]
    fn loops_and_enums() {
        let l = labels("for _, child in ipairs(workspace:GetChildren()) do\n\tchild.|\nend");
        has(&l, &["Name", "Parent"]);
        let l = labels("for i = 1, 10 do\n\t|\nend");
        has(&l, &["i"]);
        let l = labels("Enum.|");
        has(&l, &["Material", "KeyCode"]);
        let l = labels("local m = Enum.Material.|");
        has(&l, &["Plastic", "Neon"]);
        let l = labels("workspace.Baseplate.Material.|");
        has(&l, &["Name", "Value"]);
    }

    #[test]
    fn works_while_the_file_does_not_parse() {
        let l = labels("local p = workspace.Baseplate\nif p then\n\tp.|");
        has(&l, &["Anchored"]);
        let l = labels("local p = workspace.Baseplate\nlocal function f()\n\tlocal x = p.|\nend");
        has(&l, &["Anchored"]);
        let l = labels("local p = workspace.Baseplate\np.Touched:Connect(function(hit)\n\tlocal h = hit.|\n");
        has(&l, &["Anchored"]);
    }

    #[test]
    fn no_completions_in_comments_strings_or_names() {
        assert!(labels("-- workspace.|").is_empty());
        assert!(labels("print(\"workspace.|\")").is_empty());
        assert!(labels("local |").is_empty());
        assert!(labels("local x = 1.|").is_empty());
    }

    #[test]
    fn hovers() {
        let h = hover_at("local p = workspace.Baseplate\np.Anch|ored = true").unwrap();
        assert!(h.contains("BasePart.Anchored: boolean"), "{h}");
        let h = hover_at("local p = workspace.Baseplate\np|.Anchored = true").unwrap();
        assert!(h.contains("local p: Part"), "{h}");
        let h = hover_at("local Util = require(game.ReplicatedStorage.Util)\nUtil.gr|eet('a')").unwrap();
        assert!(h.contains("greet(name: string): string"), "{h}");
        let h = hover_at("workspace.Baseplate.Touched:Connect(function(h|it) end)").unwrap();
        assert!(h.contains("(parameter) hit: BasePart"), "{h}");
    }

    #[test]
    fn closes_blocks() {
        use crate::features::block_closer;
        // `|` is where Enter left the cursor.
        let close = |src: &str| {
            let at = src.find('|').unwrap();
            let text = src.replace('|', "");
            let line_end = text[..at].rfind('\n').unwrap();
            let opener_start = text[..line_end].rfind('\n').map(|i| i + 1).unwrap_or(0);
            block_closer(&text, at, &text[opener_start..line_end])
        };
        assert_eq!(close("local function f()\n\t|"), Some("end"));
        assert_eq!(close("if x then\n\t|\nprint(1)"), Some("end"));
        assert_eq!(close("for i = 1, 10 do\n\t|"), Some("end"));
        assert_eq!(close("part.Touched:Connect(function(hit)\n\t|"), Some("end)"));
        assert_eq!(close("function M:Get(): number\n\t|"), Some("end"));
        // Already closed, or not an opener.
        assert_eq!(close("if x then\n\t|\nend"), None);
        assert_eq!(close("local function f()\n\t|\n\tprint(1)\nend"), None);
        assert_eq!(close("print('then')\n|"), None);
        assert_eq!(close("local s = 'do' -- then\n|"), None);
        assert_eq!(close("if a then -- check\n\t|"), Some("end"));
    }

    #[test]
    fn monaco_positions() {
        use crate::features::{byte_offset, column_of};
        let text = "ab
c€d
";
        assert_eq!(byte_offset(text, 1, 1), 0);
        assert_eq!(byte_offset(text, 2, 3), 3 + 1 + 3);
        assert_eq!(byte_offset(text, 2, 99), 3 + 5);
        assert_eq!(column_of(text, 3 + 1 + 3), 3);
    }

    #[test]
    fn signatures() {
        let run_sig = |src: &str| {
            let offset = src.find('|').unwrap();
            let text = src.replace('|', "");
            run(|ctx| signature_help(&text, offset, ctx))
        };
        let s = run_sig("workspace.Baseplate:ApplyImpulse(|").unwrap();
        assert_eq!(s.label, "ApplyImpulse(impulse: Vector3): ()");
        let s = run_sig("local v = Vector3.new(1, |").unwrap();
        assert_eq!(s.active, 1);
        assert!(s.label.starts_with("new(x: "), "{}", s.label);
        let s = run_sig("game:GetService(\"Players\").PlayerAdded:Connect(|").unwrap();
        assert!(s.label.contains("player: Player"), "{}", s.label);
        assert!(run_sig("print(\"a\")|").is_none());
    }
}
