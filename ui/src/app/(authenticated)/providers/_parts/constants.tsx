import React from "react";
import { Link2, Mic, Volume2, Eye, Image as ImageIcon, Brain, Search } from "lucide-react";
import type { CreateProviderInput } from "@/types/api";

export const ALL_CATEGORIES = ["text", "stt", "tts", "vision", "imagegen", "embedding", "websearch"] as const;
export type ProviderCategory = typeof ALL_CATEGORIES[number];

export const ALL_CAPABILITIES = ["stt", "tts", "vision", "imagegen", "embedding", "websearch"] as const;

export const CATEGORY_BADGE_CLASS: Record<ProviderCategory, string> = {
  text: "bg-amber-500/10 text-amber-600 dark:text-amber-400 border-amber-500/20",
  stt: "bg-blue-500/10 text-blue-500 dark:text-blue-400 border-blue-500/20",
  tts: "bg-success/10 text-success border-success/20",
  vision: "bg-purple-500/10 text-purple-500 dark:text-purple-400 border-purple-500/20",
  imagegen: "bg-orange-500/10 text-orange-500 dark:text-orange-400 border-orange-500/20",
  embedding: "bg-cyan-500/10 text-cyan-500 dark:text-cyan-400 border-cyan-500/20",
  websearch: "bg-teal-500/10 text-teal-600 dark:text-teal-400 border-teal-500/20",
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