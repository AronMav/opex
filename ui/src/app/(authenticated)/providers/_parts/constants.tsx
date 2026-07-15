import React from "react";
import { Link2, Mic, Volume2, Eye, Image as ImageIcon, Brain, Search } from "lucide-react";
import type { CreateProviderInput } from "@/types/api";

export const ALL_CATEGORIES = ["text", "stt", "tts", "vision", "imagegen", "embedding", "websearch"] as const;
export type ProviderCategory = typeof ALL_CATEGORIES[number];

// Capabilities that still get an "Active provider(s)" group with a
// priority-ordered drag list on the Providers page. Profiles now own
// agent-facing routing for stt/tts/vision/imagegen/websearch (per-profile
// slots), so `embedding` is the only capability left with a *global* active
// provider — the backend rejects PUT /api/provider-active for any other
// capability (400).
export const ALL_CAPABILITIES = ["embedding"] as const;

export const CATEGORY_ICONS: Record<ProviderCategory, React.ReactNode> = {
  text: <Link2 className="h-3.5 w-3.5" />,
  stt: <Mic className="h-3.5 w-3.5" />,
  tts: <Volume2 className="h-3.5 w-3.5" />,
  vision: <Eye className="h-3.5 w-3.5" />,
  imagegen: <ImageIcon className="h-3.5 w-3.5" />,
  embedding: <Brain className="h-3.5 w-3.5" />,
  websearch: <Search className="h-3.5 w-3.5" />,
};

export const EMPTY_FORM: CreateProviderInput = {
  name: "",
  type: "",
  provider_type: "",
  base_url: "",
  default_model: "",
  notes: "",
  enabled: true,
};