"use client";

import { Button } from "@/components/ui/button";
import { ErrorState } from "@/components/ui/error-state";
import { useTranslation } from "@/hooks/use-translation";

export default function AuthenticatedError({
  error,
  reset,
}: {
  error: Error & { digest?: string };
  reset: () => void;
}) {
  const { t } = useTranslation();
  return (
    <ErrorState
      message={error.message}
      action={
        <Button onClick={reset} variant="outline" size="sm">
          {t("error.retry")}
        </Button>
      }
    />
  );
}
