import { Trans, useLingui } from "@lingui/react/macro";
import { useMutation, useQuery } from "@tanstack/react-query";

import { connectLocalLibrary, execute } from "@anlg/plugin-db";
import { Button } from "@anlg/ui/components/ui/button";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@anlg/ui/components/ui/dialog";
import { sonnerToast } from "@anlg/ui/components/ui/toast";

import {
  GlassDialogCancelButton,
  GlassDialogContent,
} from "~/shared/ui/glass-dialog";

export function ConnectLocalLibraryDialog({
  open,
  onOpenChange,
  accountUserId,
  email,
  onConnected,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  accountUserId: string;
  email: string;
  onConnected: () => Promise<void>;
}) {
  const { t } = useLingui();
  const library = useQuery({
    queryKey: ["local-library-connection", accountUserId],
    enabled: open,
    staleTime: 0,
    queryFn: async () => {
      const [row] = await execute<{ workspace_id: string }>(
        "SELECT json_extract(value_json, '$.workspace_id') AS workspace_id FROM app_settings WHERE id = 'cloudsync_workspace_binding'",
        [],
      );
      if (!row?.workspace_id) throw new Error("Local library unavailable");
      return row.workspace_id;
    },
  });
  const connect = useMutation({
    mutationFn: async () => {
      if (!library.data) throw new Error(t`Could not read the local library`);
      await connectLocalLibrary(accountUserId, library.data);
      await onConnected();
      onOpenChange(false);
    },
    onError: () =>
      sonnerToast.error(
        t`Could not connect this library. Your local notes are unchanged. Try again.`,
      ),
  });
  return (
    <Dialog
      open={open}
      onOpenChange={connect.isPending ? undefined : onOpenChange}
    >
      <GlassDialogContent>
        <DialogHeader>
          <DialogTitle>
            <Trans>Sync this local library with {email}?</Trans>
          </DialogTitle>
          <DialogDescription>
            <Trans>
              Your personal notes and recordings stay on this device. Sync will
              connect them to this account and bring in its existing notes.
              Copies already in your previous account remain there. Team and
              shared notes stay with their workspace. Reconnect integrations for
              this account. After connecting, this library requires an app
              version that supports account switching.
            </Trans>
          </DialogDescription>
        </DialogHeader>
        {library.isError && (
          <p role="alert">
            <Trans>
              Could not read the local library. Close this dialog and try again.
            </Trans>
          </p>
        )}
        <DialogFooter>
          <GlassDialogCancelButton
            disabled={connect.isPending}
            onClick={() => onOpenChange(false)}
          >
            <Trans>Keep using locally</Trans>
          </GlassDialogCancelButton>
          <Button
            disabled={!library.data || library.isFetching || connect.isPending}
            onClick={() => connect.mutate()}
          >
            {connect.isPending ? (
              <Trans>Connecting…</Trans>
            ) : (
              <Trans>Connect library</Trans>
            )}
          </Button>
        </DialogFooter>
      </GlassDialogContent>
    </Dialog>
  );
}
