import { createContext, useContext } from "react";

import type { Connection, Grafeno } from "./api.ts";

export interface ConfirmOptions {
  title: string;
  message?: string;
  confirm?: string;
  danger?: boolean;
}

export interface NewNodeOptions {
  labels?: string[];
  onCreated?: (id: string) => void;
}

export interface NewEdgeOptions {
  from?: { id: string; name: string };
  to?: { id: string; name: string };
  onCreated?: (id: string) => void;
}

export interface Studio {
  client: Grafeno;
  connection: Connection;
  namespace: string;
  setNamespace: (name: string) => void;
  namespaces: string[];
  /** Node and edge labels of the namespace, with their counts. */
  schema: { labels: Record<string, number>; edgeLabels: Record<string, number> };
  /** Changes whenever data was written from the Studio. */
  revision: number;
  /** Call after a write: reloads namespaces and labels, and tells the views. */
  changed: () => void;
  toast: (message: string, kind?: "ok" | "error" | "info") => void;
  confirm: (options: ConfirmOptions) => Promise<boolean>;
  newNode: (options?: NewNodeOptions) => void;
  newEdge: (options?: NewEdgeOptions) => void;
}

export const StudioContext = createContext<Studio | null>(null);

export function useStudio(): Studio {
  const studio = useContext(StudioContext);
  if (!studio) throw new Error("useStudio outside the Studio");
  return studio;
}
