import { GrafenoError, type Edge, type EdgeFilter, type Namespace, type Node, type Subgraph } from "../../../clients/typescript/src/index.ts";

export const ROOTS_PER_BATCH = 8;
const LEGACY_CONCURRENCY = 4;
const MAX_NEW = 150;

type Graph = Pick<Namespace, "neighborhoods" | "subgraph">;

/** Reads roots in order; aborting sends no more batches and drops late answers. */
export async function loadExpandedGraph(graph: Graph, roots: string[], filter: EdgeFilter | undefined, signal: AbortSignal) {
  const nodes = new Map<string, Node>();
  const edges = new Map<string, Edge>();
  let legacy = false;

  const individual = async (seeds: string[]): Promise<Subgraph[]> => {
    const results: Subgraph[] = Array(seeds.length);
    let cursor = 0;
    let failed = false;
    await Promise.all(Array.from({ length: Math.min(LEGACY_CONCURRENCY, seeds.length) }, async () => {
      while (!failed && cursor < seeds.length) {
        signal.throwIfAborted();
        const index = cursor++;
        try {
          results[index] = await graph.subgraph([seeds[index]], { depth: 1, filter });
        } catch (error) {
          failed = true;
          throw error;
        }
      }
    }));
    return results;
  };

  for (let start = 0; start < roots.length; start += ROOTS_PER_BATCH) {
    signal.throwIfAborted();
    const seeds = roots.slice(start, start + ROOTS_PER_BATCH);
    let around: Subgraph[];
    if (legacy) {
      around = await individual(seeds);
    } else {
      try {
        around = await graph.neighborhoods(seeds, { filter });
      } catch (error) {
        signal.throwIfAborted();
        // A node's 404 or a permission failure does not mean an older API.
        if (!(error instanceof GrafenoError) || error.code !== "http" || ![404, 405].includes(error.status)) throw error;
        legacy = true;
        around = await individual(seeds);
      }
    }
    signal.throwIfAborted();
    if (around.length !== seeds.length) throw new Error("Neighborhood response count does not match the requested roots");
    for (const [index, neighborhood] of around.entries()) {
      let added = 0;
      for (const node of neighborhood.nodes) {
        if (!nodes.has(node.id) && (added < MAX_NEW || node.id === seeds[index])) {
          nodes.set(node.id, node);
          added++;
        }
      }
      for (const edge of neighborhood.edges) edges.set(edge.id, edge);
    }
  }
  signal.throwIfAborted();
  return { nodes, edges };
}
