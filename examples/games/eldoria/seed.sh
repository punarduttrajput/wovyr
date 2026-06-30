#!/usr/bin/env bash
# Seed the "Kingdom of Eldoria" game knowledge base into Apex memory, one record per
# entity (good RAG: each query retrieves just the relevant chunk, not the whole KB).
#
# Usage:
#   ./seed.sh                 # seeds namespace "eldoria-kb"
#   ./seed.sh my-namespace    # seeds a custom namespace
#
# Re-running appends duplicates — clear ~/.apex/memory/<ns>.jsonl first to reseed.
#
# The apex binary is auto-located: $APEX_BIN, else ./target/{debug,release}/apex[.exe]
# (run from the repo root), else `apex` on PATH. Build it first: cargo build -p apex-cli.
set -euo pipefail

NS="${1:-eldoria-kb}"

# Resolve the apex CLI binary.
APEX="${APEX_BIN:-}"
if [ -z "$APEX" ]; then
  for c in target/debug/apex target/release/apex target/debug/apex.exe target/release/apex.exe; do
    [ -x "$c" ] && { APEX="$c"; break; }
  done
fi
APEX="${APEX:-$(command -v apex || true)}"
if [ -z "$APEX" ]; then
  echo "error: apex binary not found. Build it first:" >&2
  echo "  cargo build -p apex-cli" >&2
  echo "then re-run from the repo root, or set APEX_BIN=/path/to/apex." >&2
  exit 1
fi
echo "Using apex: $APEX"

# Seeding embeds each record via the gateway. A chat-only endpoint (e.g. Ollama's
# glm-4.7) has no /embeddings model, so a real key makes every `put` fail — and `set -e`
# would abort on the first one, storing nothing. Unset the provider here so embeddings
# use the offline/deterministic path; this KB is retrieved by KEYWORD, which never uses
# the embedding vector anyway. (The chat model is only needed later, at agent-run time.)
unset OPENAI_API_KEY APEX_OPENAI_BASE_URL

# put <tag> <content> [importance]
put() {
  "$APEX" memory put --namespace "$NS" --tag "$1" --content "$2" ${3:+--importance "$3"}
}

echo "Seeding '$NS'…"

# --- game / lore -------------------------------------------------------------
put game "Game: Kingdom of Eldoria (id fantasy-kingdom), a Fantasy RPG, version 1.0." 0.7
put lore "World: Eldoria is an ancient kingdom divided into five regions. It was once protected by the Crystal Guardians until the Shadow King shattered the Eternal Crystal. Players travel across the kingdom to restore balance." 0.8
put history "History year 0: Creation of the Eternal Crystal."
put history "History year 500: Rise of the Crystal Guardians."
put history "History year 1120: The Shadow King invades Eldoria."
put history "History year 1125: The Eternal Crystal is shattered into five fragments."

# --- regions -----------------------------------------------------------------
put region "Region: Emerald Forest (id forest) — a dense magical forest. Level range 1-10."
put region "Region: Ashen Desert (id desert) — a dangerous desert inhabited by sand beasts. Level range 10-20."
put region "Region: Frost Mountains (id mountains) — snow-covered mountains. Level range 20-35."
put region "Region: Infernal Peak (id volcano) — a lava-filled volcanic region. Level range 35-50."
put region "Region: Crystal City (id capital) — the capital of Eldoria."

# --- factions ----------------------------------------------------------------
put faction "Faction: Crystal Guardians — alignment Good, led by Seraphina."
put faction "Faction: Shadow Legion — alignment Evil, led by the Shadow King."
put faction "Faction: Merchant Guild — alignment Neutral, led by Marcus."

# --- characters --------------------------------------------------------------
put character "Character: Aric (id hero) — Human Knight from Crystal City. The main protagonist."
put character "Character: Seraphina (id seraphina) — Elf Mage, leader of the Crystal Guardians."
put character "Character: Malzor, the Shadow King (id shadow_king) — a Demon, the main boss."
put character "Character: Borin (id blacksmith) — a Dwarf blacksmith."

# --- npcs --------------------------------------------------------------------
put npc "NPC: Elder Rowan — Village Elder in the Emerald Forest. Gives the quests 'Forest Awakening' and 'Missing Hunter'."
put npc "NPC: Lyria — a merchant in Crystal City."
put npc "NPC: Borin — blacksmith at the Iron Forge."

# --- monsters ----------------------------------------------------------------
put monster "Monster: Slime — level 1, HP 50, weakness Fire."
put monster "Monster: Goblin — level 4, HP 120, weakness Ice."
put monster "Monster: Forest Wolf — level 6, HP 180, weakness Lightning."
put monster "Monster: Sand Golem — level 18, HP 1200, weakness Water."
put monster "Monster: Ice Dragon — level 45, HP 12000, weakness Fire."

# --- bosses ------------------------------------------------------------------
put boss "Boss: Ancient Treant — found in the Emerald Forest, level 12."
put boss "Boss: Fire Titan — found at Infernal Peak, level 42."
put boss "Boss: Shadow King — found in the Crystal Palace, level 50. The final boss."

# --- weapons / armor ---------------------------------------------------------
put weapon "Weapon: Wooden Sword — attack 5, rarity Common."
put weapon "Weapon: Iron Sword — attack 18, rarity Common."
put weapon "Weapon: Crystal Blade — attack 85, rarity Epic."
put weapon "Weapon: Shadow Slayer — attack 120, rarity Legendary."
put armor "Armor: Leather Armor — defense 8."
put armor "Armor: Steel Armor — defense 30."
put armor "Armor: Guardian Armor — defense 95."

# --- items / crafting --------------------------------------------------------
put item "Item: Health Potion — restores 100 HP."
put item "Item: Mana Potion — restores 100 MP."
put item "Item: Crystal Fragment — a quest item."
put item "Item: Teleport Scroll — returns you to the nearest town."
put crafting "Crafting recipe: Iron Sword requires 10 Iron Ore and 2 Wood."
put crafting "Crafting recipe: Crystal Blade requires 5 Crystal Fragments and 3 Mythril."

# --- skills ------------------------------------------------------------------
put skill "Knight skills: Slash, Shield Bash, Charge."
put skill "Mage skills: Fireball, Ice Lance, Meteor."
put skill "Archer skills: Rapid Shot, Poison Arrow, Eagle Eye."

# --- quests ------------------------------------------------------------------
put quest "Quest q001 'Forest Awakening' (giver: Elder Rowan): kill 10 Slimes. Reward: 100 gold, 300 XP."
put quest "Quest q002 'Missing Hunter' (giver: Elder Rowan): rescue the Hunter. Reward: an Iron Sword."
put quest "Quest q003 'Crystal Restoration' (giver: Seraphina): collect 5 Crystal Fragments."

# --- shops / travel / misc ---------------------------------------------------
put merchant "Merchant: Lyria sells Health Potion, Mana Potion, and Teleport Scroll."
put fast_travel "Fast-travel points: Crystal City, Emerald Forest, Frost Mountains, Infernal Peak. Unlock a town by visiting it once."
put achievement "Achievements: First Blood, Dragon Slayer, Master Blacksmith, Hero of Eldoria."
put controls "Controls: move WASD, attack Left Mouse, block Right Mouse, inventory I, map M, skills 1-5."

# --- dialogue ----------------------------------------------------------------
put dialogue "Elder Rowan greeting: 'Welcome, traveler. The forest has become dangerous.' On quest complete: 'Thank you for saving our village.'"
put dialogue "Seraphina greeting: 'The Crystal Guardians need your help.'"
put dialogue "Generic merchant greeting: 'Looking to buy or sell?'"

# --- tips / faq / glossary ---------------------------------------------------
put tip "Gameplay tip: Fire is effective against Ice monsters."
put tip "Gameplay tip: Water attacks defeat lava enemies faster."
put tip "Gameplay tip: Complete side quests to level up quickly."
put tip "Gameplay tip: Upgrade weapons every 10 levels."
put tip "Gameplay tip: Dodge boss attacks instead of blocking."
put faq "FAQ: How do I level up? Complete quests and defeat enemies."
put faq "FAQ: Where is the blacksmith? Borin is located at the Iron Forge."
put faq "FAQ: How do I craft weapons? Visit any blacksmith with the required materials."
put faq "FAQ: How do I unlock fast travel? Visit each town once."
put glossary "Glossary: HP = Health Points, MP = Mana Points, XP = Experience Points, NPC = Non Player Character, DPS = Damage Per Second."

echo "Done. Seeded into namespace '$NS'."
echo "Verify (offline, no embeddings):  env -u OPENAI_API_KEY $APEX memory query 'Ice Dragon weakness' --namespace $NS"
echo "Ask the guide (keyword RAG, your chat model OK):"
echo "  $APEX agents run --local -f examples/games/eldoria/game-guide.yaml --input '{\"message\":\"What is the Ice Dragon weak to?\"}'"
