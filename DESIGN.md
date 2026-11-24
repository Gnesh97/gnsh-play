# `gnsh-play` Design System Specification

**Mode:** Operate (Desktop Gaming Utility / Native Streaming Client)  
**Renderer:** `egui` 0.37 on Direct3D 11 (`gnsh-app`)  
**Scope:** Home console launcher, tabbed Settings modal, modal dialogs, and in-stream floating HUD

---

## 1. Visual Philosophy

1. **Stream First, Chrome Second:** Video content is the primary artifact. In-stream overlays stay collapsed into a single translucent floating pill unless the user explicitly opens Quick Settings (`Tune`).
2. **Cool Obsidian Surface Ladder:** Single neutral hue family (slate-navy) steps cleanly from canvas to chrome to cards without rainbow gradients or generic `#000000` voids.
3. **One Cobalt Accent, Semantic Statuses Only:** PlayStation-inspired cobalt (`#0E76D6`) denotes primary actions and active tabs/focus; emerald, amber, and crimson are reserved exclusively for readiness, live mic/lock states, standby warnings, and errors/disconnects.
4. **Concentric Geometry & Tabular Precision:** Outer containers use `12 px` corners, inner buttons/inputs use `8 px` corners, and pills use `10 px` (`<=` card corner). Live telemetry counters and MAC/IP addresses use `Monospace` so updating digits never jitter horizontally.

---

## 2. Design Tokens

### Surface Elevation Ladder
| Token | RGB / RGBA | Role |
|---|---|---|
| `CANVAS` | `#0A0D14` | Root central panel backdrop behind cards |
| `CHROME` | `#10141E` | Top application bar, modal backdrops |
| `SURFACE` | `#161B28` | Primary console cards and settings modal fill |
| `SURFACE_RAISED` | `#1E2536` | Profile strip, inputs, inactive buttons |
| `SURFACE_HOVER` | `#273046` | Hovered secondary buttons and interactive rows |
| `HUD_GLASS` | `rgba(10, 13, 20, 226)` | Floating stream HUD pill & Quick Settings drawer |

### Borders & Focus
| Token | Color | Usage |
|---|---|---|
| `BORDER_SUBTLE` | `#242D40` | 1 px hairline separating panels, cards, and HUD |
| `BORDER_STRONG` | `#36425C` | Input borders, hovered/active dividers |
| `FOCUS_RING` | `#5AA9FF` | 1.5 px keyboard/gamepad focus and active selection stroke |

### Typography & Contrast (`WCAG >= 4.5:1`)
| Token | Hex | Contrast vs `SURFACE` (`#161B28`) | Contrast vs `CANVAS` (`#0A0D14`) | Usage |
|---|---|---|---|---|
| `TEXT_PRIMARY` | `#F0F4FC` | `> 14:1` | `> 16:1` | Headings, console names, active labels |
| `TEXT_SECONDARY` | `#B0BACE` | `> 7.5:1` | `> 8.8:1` | Subtitles, setting labels, metadata |
| `TEXT_MUTED` | `#8895B0` | `>= 4.8:1` | `>= 5.4:1` | Helper descriptions, shortcut hints |

| TextStyle | Size (`px`) | Family | Role |
|---|---|---|---|
| `Heading` | `19.0` | Proportional | Window / section titles |
| `Body` | `14.0` | Proportional | Standard labels and copy |
| `Button` | `13.5` | Proportional | Buttons and segmented tabs |
| `Small` | `12.0` | Proportional | Badges, helper hints |
| `Monospace` | `12.5` | Monospace | IP/MAC IDs, live FPS/Mbps/RTT/Loss/Pipe telemetry |

### Accent & Semantic States
| Semantic Role | Fill / Bg | Foreground |
|---|---|---|
| Primary Cobalt (`ACCENT`) | `#0E76D6` (hover `#1C86E8`, active `#0B60B0`) | `#FFFFFF` |
| Ready / Live (`SUCCESS`) | `#154230` | `#7CE6AE` |
| Standby / Attention (`WARN`) | `#473314` | `#F8CA76` |
| Error / Disconnect (`DANGER`) | `#4A1C22` | `#FCA5A5` |
| Solid Disconnect CTA | `#9E2B35` | `#FFFFFF` |
| Saved / Info (`INFO`) | `#16304D` | `#9ED0FF` |

### Concentric Radii & Spacing
- `RADIUS_CARD`: `12` px (`egui::Window`, console cards, banners)
- `RADIUS_CONTROL`: `8` px (buttons, `TextEdit`, combo boxes, segmented controls)
- `RADIUS_PILL`: `10` px (`Ready`, `Standby`, `Controller`, `Live` status pills)
- Spacing defaults: `item_spacing = [10, 8]`, `button_padding = [12, 6]`, `interact_size.y = 30.0`, Home max-width column `960.0` px.

---

## 3. Structural Architecture (`crates/gnsh-app/src/ui/`)

| Module | Responsibility |
|---|---|
| `theme.rs` | Color/radius/spacing/type tokens, reusable button & frame builders, contrast verification unit tests |
| `mod.rs` | Public module exports, `SettingsTab`, `StatusBanner`, `HomeAction`, modal structs, console lookup/upsert logic |
| `home.rs` | Slim 4-item top bar (`Discover`, `Settings`, `+ Register PS5`, Controller badge), centered `960 px` Home feed, Active Stream Profile strip, composed Empty/Card states |
| `settings.rs` | Tabbed `Settings` modal (`Video`, `Audio`, `Input`), shared stream/device/KB+M widget builders, `format_profile_summary` |
| `modals.rs` | `Register PS5` modal, `Edit Saved Console` modal, `KB+M DualSense Keybindings` modal |
| `hud.rs` | Single-row translucent Streaming HUD pill + collapsible `Tune` Quick Settings drawer + Login PIN / PS5 OSK modals |
