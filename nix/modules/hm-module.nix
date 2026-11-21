self:
{ pkgs, lib, ... }:
{
  imports = [ ./hm/huddle.nix ];
  programs.huddle.package = lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.default;
}
