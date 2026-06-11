# Item Icon Prompts (English, for Gemini 2.5 Flash Image)

Ready-to-use prompts for generating the equipment-icon set from
`assets/data/equipment/`. Written for **Gemini 2.5 Flash Image ("Nano Banana")**,
which prefers full natural-language descriptions over comma-separated tag salad,
and excels at keeping a consistent style across a set.

Setting: "Age of the Schism" — late-medieval world with alchemy and **Veil Tears**
(glowing crystals used as a magic-energy resource).

---

## Workflow for a consistent set

1. **Generate the anchor first.** Run the *Style anchor* prompt below (or pick one
   simple item like the short sword) until you like the look. This image defines
   your set's style.
2. **Lock the style.** For every other item, attach the anchor image and start the
   prompt with: *"Using the attached image as the exact style reference — same flat
   dark background, same lighting, same framing, same rendering and line weight —
   generate a game item icon of: …"* then paste the item's description.
3. **Refine by conversation.** Gemini edits the current image when you reply:
   *"remove the background"*, *"make the crystal glow brighter teal"*,
   *"center it and add more empty margin"*. Use this instead of re-rolling.
4. **Background.** Ask for *"isolated on a flat dark slate background"*; for cutouts
   add *"clean silhouette, no cast shadow on the floor"* and remove the bg after.

### Style anchor prompt

> A single fantasy RPG inventory icon, late-medieval style. The object is centered
> and isolated on a flat dark slate-grey background with a subtle soft top-down
> light and a faint rim light. Painterly semi-realistic render, clean readable
> silhouette, consistent medium line weight, muted grimy color palette. Magic
> elements glow only where described — cool teal for Veil-Tear crystals, warm amber
> for Aurum craft. Square framing with even margin around the object. No text, no
> watermark, no UI border.

Keep that paragraph as a reusable **prefix**; each item below is the **subject** you
append after "generate a game item icon of:".

---

## Weapons (`weapons.toml`)

**Short Sword** (`short_sword`, 1d8)
> a plain infantry short sword: straight short steel blade, simple unadorned
> crossguard, leather-wrapped grip, disc pommel. Lightly scratched steel, dark brown
> leather. Practical and unremarkable, no shine.

**Long Sword** (`long_sword`, 2d6)
> a knightly arming sword, longer than a short sword: a central fuller down the
> blade, slightly curved guard, faceted pommel, hand-and-a-half grip. Polished steel
> with a cold highlight, dark leather grip with wire wrap. Nobler than the short
> sword.

**Staff** (`staff`, 1d6, +1 spell power)
> a tall wooden mage's staff with a gnarled forked head cradling a shard of **Veil
> Tear** — a translucent crystal with a cool teal glow. Darkened wood, a leather
> wrap, burnt runes along the shaft lit by the crystal's light. The glow is the
> focal point of the icon.

**Dagger** (`dagger`, 1d4)
> a short narrow dagger with a sharp stabbing point, tiny guard, thin grip. Matte
> dark steel, dark leather. Light and stealthy — a compact silhouette, can be angled
> like a belt knife.

**Greatsword** (`greatsword`, 2d8)
> a massive two-handed greatsword, nearly as tall as a person: broad blade, long
> guard with side parrying hooks, heavy counterweight pommel, two-hand grip. Heavy
> worn steel, scuffed leather, an imposing and crude silhouette. The largest weapon
> in the set — give it slightly more room in the frame.

**Kolm's Cleaver** (`kolm_cleaver`, 2d8, +1 strength) — *boss trophy*
> a heavy one-handed butcher's cleaver-blade: broad angled edge with a thick spine,
> nicks and stains of old blood and soot, a crude grip wrapped in tarred leather, a
> pommel of raw unworked iron. Dark blued steel with rusty red streaks. A brutal
> trophy — heavy, meaty look, a faint glint on the edge.

**Fangs** (`fangs`, 1d6) — *natural weapon, not an inventory item*
> a beast's natural weapon, not a manufactured item: a snarling open animal maw with
> bared teeth, or a pair of curved fangs, or a claw mark. Cream-yellow bone, dark
> gums. Render as a "natural attack", with no handle or metal.

**Living Lancet** (`lancet`, 1d4, +1 spell power)
> a Viridian healer's bio-instrument, grown rather than forged: a slender organic
> stylet of smooth chitin/bone with faint veins, and at the base a pulsing
> gland-"node" through which a pale-green healing light flows. Palette of bone,
> mother-of-pearl, and soft green (Viridian bio-magic, distinct from the teal of
> Veil Tears). It must read as alive — flowing lines, warm inner glow.

**Spark** (`spark_device`, 1d4, +2 spell power)
> a Hoop device-weapon, a mechanism rather than a blade: a metal frame/grip of brass
> and blued steel clutching a **Veil Tear** in faceted holding claws, with conductive
> filaments and rings running from the crystal to an emitter muzzle. The crystal
> glows bright teal with tiny electric arcs across its facets. Techno-magical look:
> rivets, gear-like parts, engraved frame. The most "instrument-like" item — focus
> on the crystal's glow and the sparks.

---

## Chest (`chest.toml`)

**Heavy Plate** (`heavy_plate`, armor 3, heavy)
> a massive plate cuirass: thick overlapping steel plates, a pronounced central
> ridge, shoulder pauldrons, rivets and straps. Polished but battle-dented steel,
> cold grey-steel sheen. The bulkiest armor — a large, monolithic silhouette.

**Plate Armor** (`plate_armor`, armor 2, heavy)
> a classic plate cuirass, thinner and lighter than the heavy plate: smooth plates,
> modest pauldrons, side straps. Clean steel with no extra decoration. The "standard
> knight's plate" — a recognizable silhouette.

**Chainmail** (`chainmail`, armor 1, medium)
> a shirt of steel rings with the characteristic mesh weave texture, short sleeves,
> a collar. Dull metal, with a dark gambeson showing under the rings. Make the ring
> texture clearly readable.

**Leather Vest** (`leather_vest`, armor 1, medium)
> a simple ranger/rogue armor of tanned leather, stitched plate-sections, front
> straps and buckles. Warm brown, worn, rough seams. A light, flexible look — soft
> folds rather than rigid metal.

**Mage Robe** (`mage_robe`, armor 0, +1 mana)
> a long cloth robe: flowing folds, wide sleeves, a hood, a cord belt. Deep
> blue/indigo with silver or teal trim and embroidered runes that faintly glow cool
> (the mana bonus). No metal — cloth and magic; a soft glow along the edges.

**Warded Jerkin** (`warded_jerkin`, armor 2, +3 HP, light) — *boss trophy*
> a quilted gambeson of dense cloth and soft leather, diamond-stitched; along the
> seams runs a thin ward-thread woven with crumbs of **Veil Tear** giving a pale
> protective glow. Faded canvas-grey with teal rune sparks. Light and understated
> but enchanted — the magic shows as the faint glowing stitching, not metal shine.

---

## Legs (`legs.toml`)

**Plate Greaves** (`plate_greaves`, armor 1, heavy)
> steel plates for thighs and shins: cuisses, knee poleyns with a domed cop, rear
> straps. Polished steel matching the plate cuirass. A rigid, segmented silhouette.

**Leather Pants** (`leather_pants`, armor 0, medium)
> sturdy traveling trousers of tanned leather with sewn-on thigh patches, straps and
> lacing. Brown leather, worn. A practical, soft look.

**Cloth Pants** (`cloth_pants`, armor 0, light)
> simple cloth trousers/baggy pants: loose cut, folds, a drawstring waist. Muted
> undyed fabric (grey-beige). The plainest legwear — no protection, just cloth.

---

## Feet (`feet.toml`)

**Iron Boots** (`iron_boots`, armor 1, heavy)
> plated sabatons: steel plates over the boot, segmented toes, a shin guard, rivets.
> Heavy steel matching the rest of the plate. A bulky, armored silhouette.

**Heavy Boots** (`heavy_boots`, armor 0, heavy)
> thick work boots of coarse leather with a heavy sole and a metal toe/heel cap,
> tight lacing or straps. Dark leather, dull metal. Sturdy but without plate.

**Leather Boots** (`leather_boots`, armor 0, medium)
> ordinary travel boots of soft leather: lacing, a neat sole, a shaft to mid-calf.
> Brown leather, light wear. A versatile, well-worn look.

**Cloth Shoes** (`cloth_shoes`, armor 0, light)
> soft fabric shoes/wraps: a thin sole, a cloth upper, ties. Pale undyed fabric. The
> lightest footwear — almost domestic, minimal detail.

---

## Notes

- **Weight reads through material color:** heavy = steel/metal, medium = leather,
  light = cloth. Keep this in the palette across icons.
- **Magic items** (`staff`, `mage_robe`, `warded_jerkin`, `spark_device`, `lancet`)
  are the only ones with a glow. Two "magic colors": teal for Veil Tears
  (Hoop/Aurum artifacts) and warm green for Viridian bio-magic (`lancet`). Keep them
  distinguishable.
- **Trophies** (`kolm_cleaver`, `warded_jerkin`) are boss rewards — worth setting
  apart with silhouette or a detail, not ordinary loot.
- **`fangs`** is not an inventory item; if an icon is needed at all, render it as a
  natural attack (maw/fang), with no handle.
