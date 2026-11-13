#!/usr/bin/env bash
# Builds the Windows bench VM in VMware Fusion (free), unattended: Windows 11 for ARM from
# Microsoft (vmw_esd2iso), a local account "sill", VMware Tools and OpenSSH.
#   bench/windows/make-vm.sh          (the VM lands in ~/Virtual Machines/sill-win)
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
fusion="/Applications/VMware Fusion.app/Contents"
vm="$HOME/Virtual Machines/sill-win"
mkdir -p "$vm" && cd "$vm"
[ -f pass.txt ] || (umask 077; LC_ALL=C tr -dc 'A-Za-z0-9' </dev/urandom | head -c 20 > pass.txt)
if ! ls *.iso >/dev/null 2>&1 || [ ! -f windows.iso ]; then
  ls *.esd >/dev/null 2>&1 || "$fusion/Public/vmw_esd2iso" getesd --edition "Professional" --lang "en-us"
  [ -d drivers ] || (mkdir drivers && unzip -oq "$fusion/Library/isoimages/arm64/drivers-arm64.zip" -d drivers)
  "$fusion/Public/vmw_esd2iso" generateiso --esd "$(ls *.esd | head -1)" --drivers "$vm/drivers/wddmvideo,$vm/drivers/vmxnet3,$vm/drivers/vmusbmouse"
  mv "$(ls -t *.iso | head -1)" windows.iso
fi
rm -rf config config.iso && mkdir config
sed "s/@PASSWORD@/$(cat pass.txt)/g" "$here/autounattend.xml" > config/autounattend.xml
cp "$here/setup.ps1" config/
hdiutil makehybrid -quiet -iso -joliet -default-volume-name SILLCFG -o config.iso config
rm -rf config
[ -f sill-win.vmdk ] || "$fusion/Library/vmware-vdiskmanager" -c -s 48GB -a nvme -t 0 sill-win.vmdk >/dev/null
cat > sill-win.vmx <<VMX
.encoding = "UTF-8"
config.version = "8"
virtualHW.version = "22"
displayName = "sill-win"
guestOS = "arm-windows11-64"
firmware = "efi"
numvcpus = "4"
cpuid.coresPerSocket = "4"
memsize = "6144"
nvram = "sill-win.nvram"
svga.present = "TRUE"
nvme0.present = "TRUE"
nvme0:0.present = "TRUE"
nvme0:0.fileName = "sill-win.vmdk"
sata0.present = "TRUE"
sata0:0.present = "TRUE"
sata0:0.deviceType = "cdrom-image"
sata0:0.fileName = "$vm/windows.iso"
sata0:1.present = "TRUE"
sata0:1.deviceType = "cdrom-image"
sata0:1.fileName = "$vm/config.iso"
sata0:2.present = "TRUE"
sata0:2.deviceType = "cdrom-image"
sata0:2.fileName = "$fusion/Library/isoimages/arm64/windows.iso"
ethernet0.present = "TRUE"
ethernet0.connectionType = "nat"
ethernet0.virtualDev = "vmxnet3"
ethernet0.addressType = "generated"
usb.present = "TRUE"
usb_xhci.present = "TRUE"
sound.present = "TRUE"
sound.virtualDev = "hdaudio"
sound.autodetect = "TRUE"
pciBridge0.present = "TRUE"
pciBridge4.present = "TRUE"
pciBridge4.virtualDev = "pcieRootPort"
pciBridge4.functions = "8"
vmci0.present = "TRUE"
hpet0.present = "TRUE"
tools.syncTime = "TRUE"
isolation.tools.hgfs.disable = "FALSE"
VMX
echo "$vm/sill-win.vmx"
