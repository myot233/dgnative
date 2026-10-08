import { PRIZES } from "./data.mjs";

export function nearestHigher(index, prizes = PRIZES) {
  const circumference = 2 * prizes.reduce((sum, p) => sum + p.weight, 0);
  let at = 0;
  const centers = prizes.map(p => { const center = at + p.weight; at += 2 * p.weight; return center; });
  const candidates = prizes.map((p, i) => {
    const distance = Math.abs(centers[i] - centers[index]);
    return { index: i, strength: p.strength_pct, distance: Math.min(distance, circumference - distance) };
  }).filter(p => p.strength > prizes[index].strength_pct);
  candidates.sort((a, b) => a.distance - b.distance || a.strength - b.strength || a.index - b.index);
  return candidates[0]?.index ?? index;
}

export function finalOdds(chance, prizes = PRIZES) {
  const total = prizes.reduce((sum, p) => sum + p.weight, 0);
  const odds = prizes.map(() => 0);
  prizes.forEach((prize, index) => {
    const higher = nearestHigher(index, prizes);
    const upgrade = higher === index ? 0 : chance / 100;
    odds[index] += prize.weight / total * (1 - upgrade);
    odds[higher] += prize.weight / total * upgrade;
  });
  return odds;
}

export function randomBelow(max, crypto = globalThis.crypto) {
  const ceiling = 2 ** 32 - (2 ** 32 % max);
  const value = new Uint32Array(1);
  do { crypto.getRandomValues(value); } while (value[0] >= ceiling);
  return value[0] % max;
}

export function draw(chance, random = randomBelow, prizes = PRIZES) {
  let roll = random(prizes.reduce((sum, p) => sum + p.weight, 0));
  const preview = prizes.findIndex(prize => { roll -= prize.weight; return roll < 0; });
  const index = random(100) < chance ? nearestHigher(preview, prizes) : preview;
  return { preview_index: preview, index, prize: prizes[index] };
}
