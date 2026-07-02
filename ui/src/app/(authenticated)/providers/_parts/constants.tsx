import React from "react";
import { Link2, Mic, Volume2, Eye, Image as ImageIcon, Brain, Search } from "lucide-react";
import type { CreateProviderInput } from "@/types/api";

export const ALL_CATEGORIES = ["text", "stt", "tts", "vision", "imagegen", "embedding", "websearch"] as const;
export type ProviderCategory = typeof ALL_CATEGORIES[number];

export const ALL_CAPABILITIES = ["stt", "tts", "vision", "imagegen", "embedding", "websearch"] as const;

// Category → semantic chart token. Chart tokens (--chart-1..8, mapped in
// globals.css @theme inline) are theme-swappable (light+dark) AND lint-clean,
// unlike raw Tailwind palette colors. Mapping keeps each category's original
// hue: text=amber(3) stt=blue(1) tts=green(2) vision=purple(5)
// imagegen=orange(7) embedding=cyan(6) websearch→indigo(8, no teal token).
export const CATEGORY_BADGE_CLASS: Record<ProviderCategory, string> = {
  text: "bg-chart-3/10 text-chart-3 border-chart-3/20",
  stt: "bg-chart-1/10 text-chart-1 border-chart-1/20",
  tts: "bg-chart-2/10 text-chart-2 border-chart-2/20",
  vision: "bg-chart-5/10 text-chart-5 border-chart-5/20",
  imagegen: "bg-chart-7/10 text-chart-7 border-chart-7/20",
  embedding: "bg-chart-6/10 text-chart-6 border-chart-6/20",
  websearch: "bg-chart-8/10 text-chart-8 border-chart-8/20",
};

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